//! The pane beside the transcript, chosen by the conversation's protocol.
//!
//! The transcript is the same for every conversation; this is where they
//! differ. An outline conversation shows its reversible change set
//! ([`super::change_set`]); an implementation conversation shows the plan it
//! is working, the latest test run its agent recorded, and the files its
//! worktree has changed. A plain chat has nothing to show.
//!
//! Spec: `doc/conversation/protocols.md` §4.5.

use super::{ConversationView, Pane};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::{h_flex, v_flex};
use tod_core::conversation::implement::TestRun;
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
            // A chat's outline writes are recorded like an outline
            // conversation's, so it has a change set too.
            ProtocolKind::Outline | ProtocolKind::Chat => self.render_change_set(window, cx),
            ProtocolKind::Implementation => self.render_implementation_pane(window, cx),
            // A stub until the designer is rebuilt as this pane. The working
            // designer is still `views::visual_design_panel`.
            ProtocolKind::VisualDesign => self.render_empty_pane(
                "Visual design",
                "The visual designer is not available in conversations yet.",
            ),
        }
    }

    /// Rows Up/Down move among in the implementation pane.
    fn side_row_count(&self) -> usize {
        self.data.plan.len() + self.side_files.len()
    }

    /// Move the implementation pane's highlight; entering an unhighlighted
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
    /// the worktree's changed files.
    fn render_implementation_pane(
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

        let mut rows: Vec<AnyElement> = Vec::new();
        for (n, step) in self.data.plan.iter().enumerate() {
            rows.push(
                h_flex()
                    .when(lit(n), style::highlighted)
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
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(selectable_text(
                                format!("plan-step-{}", step.id),
                                step.body.clone(),
                                window,
                                cx,
                            ))
                            // Why a partial or blocked step stopped, and how
                            // to unblock it: what the user acts on.
                            .children(step.note.as_ref().map(|note| {
                                style::text_dense_muted(div()).child(selectable_text(
                                    format!("plan-step-note-{}", step.id),
                                    note.clone(),
                                    window,
                                    cx,
                                ))
                            })),
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
                        .child("Implementation"),
                    )
                    .child(style::text_dense_muted(div()).child(format!("{done}/{total} steps")))
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
                    .id("implementation-pane")
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
