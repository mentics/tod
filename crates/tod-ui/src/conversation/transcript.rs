//! The transcript pane: the conversation's turns and the message input.

use super::{ConversationView, Pane, Stop};
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::Textarea;
use gpui_component::scroll::Scrollbar;
use gpui_component::{Selectable, Sizable, h_flex, v_flex};
use tod_core::conversation::ROTATION_NOTE;
use tod_store::conversation::TurnRole;

const INPUT_HEIGHT: f32 = 104.;

impl ConversationView {
    pub(super) fn render_transcript(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.pane == Pane::Transcript;
        let stop = (active && self.picker.is_none() && !self.text_editing()).then_some(self.stop);
        if self.data.turns.len() != self.rendered_turns {
            self.rendered_turns = self.data.turns.len();
            self.transcript_scroll.scroll_to_bottom();
        }

        let mut turns: Vec<AnyElement> = Vec::new();
        for (ix, turn) in self.data.turns.iter().enumerate() {
            let id = ("turn", ix);
            turns.push(match turn.role {
                TurnRole::User => style::text(div())
                    .rounded(style::radius::CONTROL)
                    .px(style::space::RELATED)
                    .py(style::space::INLINE)
                    .bg(style::color::badge_fill())
                    .child(selectable_text(id, turn.body.clone(), window, cx).w_full())
                    .into_any_element(),
                TurnRole::Agent if turn.body.trim().is_empty() => style::text_muted(div())
                    .px(style::space::RELATED)
                    .child("Done, no notes")
                    .into_any_element(),
                TurnRole::Agent => style::text(div())
                    .px(style::space::RELATED)
                    .child(selectable_markdown(id, turn.body.clone(), window, cx).w_full())
                    .into_any_element(),
                TurnRole::Error => div()
                    .px(style::space::RELATED)
                    .child(style::text_error(
                        selectable_text(id, turn.body.clone(), window, cx).w_full(),
                    ))
                    .into_any_element(),
                TurnRole::Rotation => h_flex()
                    .items_center()
                    .gap(style::space::RELATED)
                    .child(
                        div()
                            .flex_1()
                            .h(style::size::BORDER)
                            .bg(style::color::divider()),
                    )
                    .child(style::text_dense_muted(div()).child(ROTATION_NOTE))
                    .child(
                        div()
                            .flex_1()
                            .h(style::size::BORDER)
                            .bg(style::color::divider()),
                    )
                    .into_any_element(),
            });
        }
        if self.status.running {
            turns.push(
                style::text_muted(div())
                    .px(style::space::RELATED)
                    .child("Working…")
                    .into_any_element(),
            );
        }
        if turns.is_empty() {
            let message = format!(
                "No conversation about {} yet. Give direction below.",
                self.data.title
            );
            turns.push(
                style::empty_message(div())
                    .p(style::space::INSET)
                    .child(selectable_text("transcript-empty", message, window, cx))
                    .into_any_element(),
            );
        }

        let input_highlighted = stop == Some(Stop::Input);
        let field = div()
            .id("conversation-input")
            .w_full()
            .h(px(INPUT_HEIGHT))
            .overflow_hidden()
            .rounded(style::radius::CONTROL)
            .when(input_highlighted, style::highlighted)
            .on_click(cx.listener(|this, _, window, cx| this.enter_input_edit(window, cx)))
            .child(
                Textarea::new(&self.input)
                    .disabled(!self.input_editing)
                    .w_full()
                    .h(px(INPUT_HEIGHT)),
            );

        let running = self.status.running;
        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                style::panel_header(h_flex()).items_center().child(
                    if active {
                        style::text_title(div())
                    } else {
                        style::text_muted(div())
                    }
                    .child("Conversation"),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        v_flex()
                            .id("transcript-list")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.transcript_scroll)
                            .p(style::space::INSET)
                            .gap(style::space::RELATED)
                            // Turns keep their natural height; the list scrolls instead.
                            .children(
                                turns
                                    .into_iter()
                                    .map(|turn| div().w_full().flex_shrink_0().child(turn)),
                            ),
                    )
                    .child(
                        div()
                            .occlude()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.))
                            .child(Scrollbar::vertical(&self.transcript_scroll)),
                    ),
            )
            .child(
                style::panel_footer(v_flex()).child(field).child(
                    h_flex()
                        .items_center()
                        .gap(style::space::RELATED)
                        .child(style::text_dense_muted(div()).flex_1().min_w_0().child(
                            if self.input_editing {
                                "Ctrl+Enter sends · Esc stops writing"
                            } else {
                                "Enter to write · Ctrl+N new conversation"
                            },
                        ))
                        .when(running, |el| {
                            el.child(
                                Button::new("conversation-stop")
                                    .label("Stop")
                                    .ghost()
                                    .small()
                                    .selected(stop == Some(Stop::Stop))
                                    .on_click(cx.listener(|this, _, _, cx| this.stop_turn(cx))),
                            )
                        })
                        .child(
                            Button::new("conversation-send")
                                .label("Send")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                        ),
                ),
            )
            .into_any_element()
    }
}
