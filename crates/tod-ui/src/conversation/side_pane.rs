//! The pane beside the transcript, chosen by the conversation's protocol.
//!
//! The transcript is the same for every conversation; this is where they
//! differ. An outline conversation shows its reversible change set
//! ([`super::change_set`]); an implementation conversation shows the plan it
//! is working, the test status from the latest report, and the files its
//! worktree has changed. A plain chat has nothing to show.
//!
//! Spec: `doc/conversation/protocols.md` §4.6.

use super::{ConversationView, Pane};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::{h_flex, v_flex};
use tod_core::conversation::implement::Report;
use tod_store::conversation::ProtocolKind;
use tod_store::outline::repos::plan_steps::{STATUS_IMPLEMENTED, STATUS_VERIFIED};

impl ConversationView {
    /// The side pane for whichever protocol runs the open conversation.
    pub(super) fn render_side_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.data.protocol {
            ProtocolKind::Outline => self.render_change_set(window, cx),
            ProtocolKind::Implementation => self.render_implementation_pane(window, cx),
            ProtocolKind::Chat | ProtocolKind::VisualDesign => self.render_empty_pane(),
        }
    }

    /// Plan steps first — the same state the protocol's done-check reads — then
    /// the worktree's changed files.
    fn render_implementation_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.pane == Pane::ChangeSet;
        let report: Option<Report> = self
            .data
            .report
            .clone()
            .and_then(|value| serde_json::from_value(value).ok());

        let done = self
            .data
            .plan
            .iter()
            .filter(|step| step.status == STATUS_IMPLEMENTED || step.status == STATUS_VERIFIED)
            .count();
        let total = self.data.plan.len();

        let mut rows: Vec<AnyElement> = Vec::new();
        for step in &self.data.plan {
            rows.push(
                h_flex()
                    .gap(style::space::INLINE)
                    .px(style::space::RELATED)
                    .py(style::space::INLINE)
                    .items_start()
                    .child(
                        style::badge(div())
                            .flex_shrink_0()
                            .child(step.status.clone()),
                    )
                    .child(
                        div().flex_1().min_w_0().child(selectable_text(
                            format!("plan-step-{}", step.id),
                            step.body.clone(),
                            window,
                            cx,
                        )),
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
            for line in &self.side_files {
                rows.push(
                    div()
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
                        .child("Implementation"),
                    )
                    .child(
                        style::text_dense_muted(div()).child(format!("{done}/{total} steps")),
                    )
                    .when_some(report.as_ref(), |el, report| {
                        el.child(style::badge(div()).child(test_label(report)))
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
                    .id("implementation-pane")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb(style::space::RELATED)
                    .children(rows),
            )
            .when_some(report, |el, report| {
                el.when(!report.summary.trim().is_empty(), |el| {
                    el.child(
                        style::panel_footer(div()).child(selectable_text(
                            "implementation-summary",
                            report.summary.clone(),
                            window,
                            cx,
                        )),
                    )
                })
            })
            .into_any_element()
    }

    fn render_empty_pane(&self) -> AnyElement {
        v_flex()
            .size_full()
            .min_w_0()
            .child(style::panel_header(div()).child(style::text_muted(div()).child("Chat")))
            .child(
                style::empty_message(div())
                    .p(style::space::INSET)
                    .child("This conversation keeps no change set."),
            )
            .into_any_element()
    }
}

/// The report's test status, as one badge.
fn test_label(report: &Report) -> String {
    if !report.tests.ran {
        return "tests not run".to_string();
    }
    if report.tests.green {
        "tests green".to_string()
    } else {
        "tests red".to_string()
    }
}
