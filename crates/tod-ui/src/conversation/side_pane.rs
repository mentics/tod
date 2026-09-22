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

use super::{ChangeAction, ConversationView, Pane};
use crate::ui::agent_conversation::{NoticeTone, PanelNotice};
use crate::ui::item_list::{ItemListRow, ItemRowState};
use crate::ui::selectable_text::selectable_text;
use crate::ui::status_filter::{render_status_filter, status_counts};
use crate::ui::style;
use crate::views::plan_steps::MutationRouter;
use crate::views::rows::{
    FindingRowProps, RowHost, RowOptions, STATUS_COLUMN_WIDTH, StatusMenu, finding_row,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, ElementId, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, WeakEntity, Window, div, relative,
};
use gpui_component::button::Button;
use gpui_component::{Disableable, Sizable, h_flex, v_flex};
use std::collections::HashMap;
use std::rc::Rc;
use tod_core::conversation::implement::{HandoffAnswer, TestRun, handoff_answer_message};
use tod_store::conversation::ProtocolKind;
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::outline::repos::plan_steps::{
    HandoffReason, STATUS_FAILED, STATUS_IMPLEMENTED, STATUS_IN_PROGRESS, STATUS_VERIFIED,
    needs_user,
};
use tod_store::outline::{OutlineMutation, PlanStep};
use tod_store::review::{FINDING_REJECTED, FINDING_STATUSES, ReviewFinding, USER_FINDING_STATUSES};
use tod_store::verification::{
    ObligationStanding, VERDICT_FAILED, VERDICT_REOPENED, VERDICT_VERIFIED,
};
use uuid::Uuid;

/// One review finding in the side pane's list. A finding carries everything
/// its row shows, so there is nothing to hang beside it.
#[derive(Debug, Clone)]
pub(super) struct FindingItem {
    pub finding: ReviewFinding,
}

/// Findings are a flat run in the order they were recorded: a review's
/// sequence, not a hierarchy, so there is nothing to group by.
pub(super) type FindingRow = ItemListRow<FindingItem>;

/// Render one finding for the item list. `menu` is the open status dropdown,
/// whichever finding it is on, and `active` whether the pane has the
/// keyboard — the highlight only shows while it does.
pub(super) fn render_finding(
    item: &FindingItem,
    state: ItemRowState<'_>,
    menu: Option<StatusMenu>,
    active: bool,
    host: &RowHost<ChangeAction>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let props = FindingRowProps {
        finding: &item.finding,
        row_ix: state.row_ix,
        highlighted: state.highlighted && active,
        status_menu: menu,
        columns: state.columns,
    };
    finding_row(props, host, RowOptions::default(), window, cx)
}

/// What the side pane lists; see [`ConversationView::side_list`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SideList {
    ChangeSet,
    Plan,
    Findings,
    Obligations,
    /// A gate check on a node with no lifecycle state to go by.
    Gate,
    Empty(&'static str, &'static str),
}

impl ConversationView {
    /// Open the status dropdown on review finding `finding`, highlighting the
    /// status it has. A plan step's dropdown belongs to the plan list the
    /// pane hosts, which owns its own.
    pub(super) fn open_status_menu(&mut self, finding: Uuid, cx: &mut Context<Self>) {
        let Some(f) = self.data.findings.iter().find(|f| f.id == finding) else {
            return;
        };
        let options: &'static [&'static str] = if f.status == FINDING_REJECTED {
            &FINDING_STATUSES
        } else {
            &USER_FINDING_STATUSES
        };
        let menu = StatusMenu::open(finding, options, &f.status);
        self.pane = Pane::ChangeSet;
        self.picker = None;
        self.status_menu = Some(menu);
        cx.notify();
    }

    /// Move the open status dropdown's highlight; `false` when none is open.
    pub(super) fn move_status_menu(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(menu) = self.status_menu.as_mut() else {
            return false;
        };
        menu.move_highlight(delta);
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
    fn render_failure(step: &PlanStep, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
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

    /// What verification is really ruling on: the node's own obligations, each
    /// with its verdict and the evidence behind it. Shown in a verification,
    /// and in an implementation once verification has sent something back.
    fn render_requirements(
        &self,
        verifying: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let standings = &self.data.standings;
        let ruled = standings.iter().any(|s| s.verdict.is_some());
        if standings.is_empty() || !(verifying || ruled) {
            return None;
        }
        let verified = standings.iter().filter(|s| s.is_verified()).count();
        let failed = standings.iter().filter(|s| s.is_failed()).count();
        let mut summary = format!("{verified}/{} verified", standings.len());
        if failed > 0 {
            summary.push_str(&format!(", {failed} failed"));
        }
        let filter = self.render_obligation_filter(
            "requirement",
            &[
                "unchecked",
                VERDICT_REOPENED,
                VERDICT_FAILED,
                VERDICT_VERIFIED,
            ],
            |s| s.status().to_string(),
            cx,
        );
        let shown = standings
            .iter()
            .filter(|s| self.obligation_filter.admits(s.status()));
        let rows = shown.map(|standing| {
            let id = standing.obligation.id;
            let badge = style::badge(div()).child(standing.status().to_string());
            let badge = if standing.is_failed() {
                style::text_error(badge)
            } else {
                badge
            };
            h_flex()
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
                            format!("requirement-{id}"),
                            standing.obligation.body.clone(),
                            window,
                            cx,
                        ))
                        .children(standing.verdict.as_ref().map(|verdict| {
                            style::text_dense_muted(div()).child(selectable_text(
                                format!("requirement-evidence-{id}"),
                                verdict.evidence.clone(),
                                window,
                                cx,
                            ))
                        })),
                )
                .into_any_element()
        });
        let rows: Vec<AnyElement> = rows.collect();
        Some(
            v_flex()
                .flex_shrink_0()
                .max_h(relative(0.5))
                .min_h_0()
                .child(
                    h_flex()
                        .gap(style::space::INLINE)
                        .px(style::space::RELATED)
                        .pt(style::space::RELATED)
                        .child(style::text_dense_muted(div()).child("Requirements"))
                        .child(style::text_dense_muted(div()).child(summary)),
                )
                .children(filter)
                .when(rows.is_empty(), |el| {
                    el.child(
                        style::empty_message(div())
                            .px(style::space::RELATED)
                            .py(style::space::INLINE)
                            .child("No requirements in the chosen statuses."),
                    )
                })
                .child(
                    v_flex()
                        .id("requirements-pane")
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(rows),
                )
                .child(
                    style::text_dense_muted(div())
                        .px(style::space::RELATED)
                        .pt(style::space::RELATED)
                        .child("Plan steps"),
                )
                .into_any_element(),
        )
    }

    /// Under a step left for the user: why, what is left, and a way to answer
    /// for each reason — keep one of the conflicting obligations, choose an
    /// option, or retry once the access or outside thing is in place.
    fn render_handoff(
        view: &WeakEntity<Self>,
        cited: &HashMap<Uuid, String>,
        step: &PlanStep,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        if !needs_user(&step.status) {
            return None;
        }
        let id = step.id;
        let answer_button = |key: String, label: &'static str, answer: HandoffAnswer| {
            let view = view.clone();
            Button::new(ElementId::Name(key.into()))
                .label(label)
                .small()
                .flex_shrink_0()
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.answer_handoff(id, answer.clone(), cx);
                    });
                })
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
                    let text = match cited.get(obligation) {
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
                )));
            }
            // Handed back before reasons existed: the note is all there is,
            // and the message input answers it.
            None => {}
        }
        Some(col.into_any_element())
    }

    /// The side pane for whichever protocol runs the open conversation: the
    /// list it works on top and, once the node has a gate check verdict, that
    /// verdict beneath it.
    pub(super) fn render_side_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let list = self.side_list();
        let top = match list {
            SideList::ChangeSet => self.render_change_set(window, cx),
            SideList::Plan => self.render_plan_pane(window, cx),
            SideList::Findings => self.render_review_pane(window, cx),
            SideList::Obligations => self.render_obligations_pane(window, cx),
            SideList::Gate => return self.render_gate_pane(window, cx),
            SideList::Empty(title, message) => self.render_empty_pane(title, message),
        };
        let notices = self.gate_notices(cx);
        if notices.is_empty() {
            return top;
        }
        let verdict = self.render_gate_section(notices, window, cx);
        v_flex()
            .size_full()
            .min_w_0()
            .child(div().flex_1().min_h_0().child(top))
            .child(
                div()
                    .flex_shrink_0()
                    .max_h(relative(0.45))
                    .border_t_1()
                    .border_color(style::color::divider())
                    .child(verdict),
            )
            .into_any_element()
    }

    /// Which list the side pane shows. A protocol that works a list shows
    /// that list; a gate check or a state's entry shows the list the node's
    /// lifecycle state is about — obligations while it is being specified,
    /// the plan from planning through verification, the findings in review —
    /// so the user can see where those items stand. (The gate check's own
    /// verdict sits beneath the list.)
    pub(super) fn side_list(&self) -> SideList {
        match self.data.protocol {
            // A chat's outline writes are recorded like an outline
            // conversation's, so it has a change set too.
            ProtocolKind::Outline | ProtocolKind::Chat => SideList::ChangeSet,
            ProtocolKind::Implementation | ProtocolKind::Verification => SideList::Plan,
            ProtocolKind::Review | ProtocolKind::Fix => SideList::Findings,
            ProtocolKind::GateCheck | ProtocolKind::OnEntry => {
                match self.data.lifecycle.as_ref().map(|s| s.lifecycle.as_str()) {
                    Some("proposed" | "design") => SideList::Obligations,
                    Some("planning" | "ready" | "active" | "verifying") => SideList::Plan,
                    Some("review" | "approved") => SideList::Findings,
                    _ if self.data.protocol == ProtocolKind::GateCheck => SideList::Gate,
                    _ => SideList::Empty(
                        "On entry",
                        "What the state's agent did is in the transcript.",
                    ),
                }
            }
            ProtocolKind::Incoming => SideList::Obligations,
            // A stub until the designer is rebuilt as this pane. The working
            // designer is still `views::visual_design_panel`.
            ProtocolKind::VisualDesign => SideList::Empty(
                "Visual design",
                "The visual designer is not available in conversations yet.",
            ),
        }
    }

    /// The node's own obligations: the real obligations panel, hosted here,
    /// so they group, edit and reorder exactly as they do on the node tree.
    /// Each row carries its standing — its verdict, else whether a plan step
    /// satisfies it — as a trailing badge.
    fn render_obligations_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(node) = self.data.focus_node else {
            return self.render_empty_pane("Obligations", "This conversation is not about a node.");
        };
        let title = self.data.title.clone();
        self.side_obligations.update(cx, |list, cx| {
            list.retarget(node, &title, None, false, window, cx);
        });
        let standings = &self.data.standings;
        let unplanned = standings
            .iter()
            .filter(|s| !self.data.planned.contains(&s.obligation.id))
            .count();
        let summary = match (standings.len(), unplanned) {
            (0, _) => "none yet".to_string(),
            (n, 0) => format!("{n}, all in the plan"),
            (n, u) => format!("{n}, {u} not in the plan"),
        };
        v_flex()
            .size_full()
            .min_w_0()
            .child(
                style::panel_header(div()).child(
                    h_flex()
                        .gap(style::space::INLINE)
                        .child(style::text_muted(div()).child("Obligations"))
                        .child(style::text_dense_muted(div()).child(summary)),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.side_obligations.clone()),
            )
            .into_any_element()
    }

    /// The review findings the status filter lets through, in the order they
    /// were recorded.
    pub(super) fn shown_findings(&self) -> Vec<ReviewFinding> {
        self.data
            .findings
            .iter()
            .filter(|f| self.status_filter.admits(&f.status))
            .cloned()
            .collect()
    }

    /// Show or hide review findings in `status`. With no status toggled on,
    /// every row shows.
    pub(super) fn toggle_status_filter(&mut self, status: &str, cx: &mut Context<Self>) {
        self.status_filter.toggle(status);
        // The list keeps its cursor on the finding it was on when that row
        // still shows, and falls back to the first one when it does not.
        self.status_menu = None;
        cx.notify();
    }

    /// One toggle per status the review pane has a finding in, with its
    /// count, plus "All" to clear them. The plan pane's own filter belongs to
    /// the list it hosts.
    fn render_status_filter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let counts = status_counts(
            &FINDING_STATUSES,
            self.data.findings.iter().map(|f| f.status.as_str()),
        );
        render_status_filter(
            "finding",
            &counts,
            &self.status_filter,
            |this: &mut Self, status, _, cx| match status {
                Some(status) => this.toggle_status_filter(status, cx),
                None => {
                    if this.status_filter.clear() {
                        cx.notify();
                    }
                }
            },
            cx,
        )
    }

    /// The same for an obligations list, by each obligation's standing
    /// (`status_of`).
    fn render_obligation_filter(
        &self,
        id: &str,
        order: &[&str],
        status_of: impl Fn(&ObligationStanding) -> String,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let statuses: Vec<String> = self.data.standings.iter().map(status_of).collect();
        let counts = status_counts(order, statuses.iter().map(String::as_str));
        render_status_filter(
            id,
            &counts,
            &self.obligation_filter,
            |this: &mut Self, status, _, cx| {
                match status {
                    Some(status) => this.obligation_filter.toggle(status),
                    None => {
                        this.obligation_filter.clear();
                    }
                }
                cx.notify();
            },
            cx,
        )
    }

    /// Move the review pane's highlight, which the findings list owns.
    pub(super) fn move_side_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.findings.move_cursor(delta as i32) {
            cx.notify();
        }
    }

    /// The finding under the review pane's highlight.
    pub(super) fn highlighted_finding(&self) -> Option<Uuid> {
        self.findings.cursor_item().map(|item| item.finding.id)
    }

    /// The plan the conversation is working: the real plan-steps panel,
    /// hosted here, so a step affords the same editing, creation, reordering
    /// and deletion as it does on the node tree, and its status chip opens
    /// the same dropdown. Around it sit what only a conversation knows — the
    /// test run its agent recorded, what verification ruled on the node's
    /// obligations, and the files its worktree has changed. Verification
    /// fails steps back to implementation, not to the user, so it offers no
    /// answers to a step left for the user.
    fn render_plan_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(node) = self.data.focus_node else {
            return self
                .render_empty_pane("Implementation", "This conversation is not about a node.");
        };
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

        let requirements = self.render_requirements(verifying, window, cx);
        let files = self.render_changed_files(window, cx);

        // A user edit here is the user's own action on the conversation, so
        // it joins the change set and can be reversed; before the first
        // message there is no conversation to record it in.
        let router: MutationRouter = match self.conversation_id {
            Some(conversation_id) => Rc::new(move |mutation| InterviewCommand::ConversationEdit {
                conversation_id,
                mutation,
            }),
            None => Rc::new(|mutation| InterviewCommand::Outline {
                mutation,
                target: None,
            }),
        };
        let view = cx.weak_entity();
        let cited = self.data.cited.clone();
        let title = self.data.title.clone();
        self.side_plan.update(cx, |list, cx| {
            list.set_mutation_router(Some(router));
            list.set_row_detail(Some(Rc::new(move |step, window, cx| {
                if verifying {
                    Self::render_failure(step, window, cx)
                } else {
                    Self::render_handoff(&view, &cited, step, window, cx)
                        .or_else(|| Self::render_failure(step, window, cx))
                }
            })));
            list.retarget(node, &title, false, window, cx);
        });

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
            .children(requirements)
            .child(div().flex_1().min_h_0().child(self.side_plan.clone()))
            .children(files)
            .into_any_element()
    }

    /// The files the conversation's worktree has changed: its own section
    /// under the plan, since they are about the run, not about a step.
    fn render_changed_files(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.side_files.is_empty() {
            return None;
        }
        let rows = self.side_files.iter().map(|line| {
            div()
                .px(style::space::RELATED)
                .py(style::space::INLINE)
                .child(selectable_text(
                    format!("worktree-{line}"),
                    line.clone(),
                    window,
                    cx,
                ))
                .into_any_element()
        });
        let rows: Vec<AnyElement> = rows.collect();
        Some(
            v_flex()
                .flex_shrink_0()
                .max_h(relative(0.3))
                .min_h_0()
                .border_t_1()
                .border_color(style::color::divider())
                .child(
                    style::text_dense_muted(div())
                        .px(style::space::RELATED)
                        .pt(style::space::RELATED)
                        .child("Changed files"),
                )
                .child(
                    v_flex()
                        .id("changed-files")
                        .min_h_0()
                        .overflow_y_scroll()
                        .pb(style::space::RELATED)
                        .children(rows),
                )
                .into_any_element(),
        )
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
        // A menu left open on a finding that has since gone closes.
        if self
            .status_menu
            .is_some_and(|m| !self.data.findings.iter().any(|f| f.id == m.item))
        {
            self.status_menu = None;
        }
        let rows: Vec<FindingRow> = self
            .shown_findings()
            .into_iter()
            .map(|finding| ItemListRow::item(finding.id.to_string(), FindingItem { finding }))
            .collect();
        let empty = rows.is_empty();
        self.findings.set_rows(rows);

        let list = if empty {
            style::empty_message(div())
                .p(style::space::INSET)
                .child(if total == 0 {
                    "No findings recorded."
                } else {
                    "No findings in the chosen statuses."
                })
                .into_any_element()
        } else {
            let menu = self.status_menu;
            let row_host = self.host.clone();
            self.findings.render(
                "review-pane",
                &self.host,
                move |item, state, window, cx| {
                    render_finding(item, state, menu, active, &row_host, window, cx)
                },
                window,
                cx,
            )
        };

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
            .child(list)
            .into_any_element()
    }

    /// The gate check's verdict: why, each blocker with the button that acts
    /// on it, a Waive per failing criterion, and the recommended next step.
    fn render_gate_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let notices = self.gate_notices(cx);
        if notices.is_empty() {
            let message = if self.status.running {
                "The verdict will appear here."
            } else {
                "No verdict yet."
            };
            return self.render_empty_pane("Gate check", message);
        }
        self.render_gate_section(notices, window, cx)
    }

    /// The gate check's verdict rows under a "Gate check" header, scrolling
    /// within whatever height the pane gives them.
    fn render_gate_section(
        &mut self,
        notices: Vec<PanelNotice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
