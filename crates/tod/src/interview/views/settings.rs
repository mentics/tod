use crate::interview::{TodPaths, TodSettings};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px,
};
use gpui_component::input::{
    InputEvent, InputState, NumberInput, NumberInputEvent, StepAction,
};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};

pub struct SettingsView {
    paths: TodPaths,
    settings: TodSettings,
    replenish_input: Entity<InputState>,
    second_input: Entity<InputState>,
    focus_handle: FocusHandle,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let settings = TodSettings::load(&paths).expect("failed to load tod settings");

        let replenish_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.researcher.replenish_threshold.to_string())
        });
        let second_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.researcher.second_researcher_threshold.to_string())
        });

        let focus_handle = cx.focus_handle();
        let view = Self {
            paths,
            settings: settings.clone(),
            replenish_input: replenish_input.clone(),
            second_input: second_input.clone(),
            focus_handle,
        };

        cx.subscribe(&replenish_input, |this, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                if let Ok(value) = text.parse::<u32>() {
                    this.settings.researcher.replenish_threshold = value;
                    let _ = this.settings.save(&this.paths);
                }
            }
        })
        .detach();

        cx.subscribe_in(&replenish_input, window, |this, input, event, window, cx| {
            let NumberInputEvent::Step(action) = event;
            if let Some(value) = stepped_value(&input.read(cx).value(), *action) {
                input.update(cx, |input, cx| {
                    input.set_value(value.to_string(), window, cx);
                });
                this.settings.researcher.replenish_threshold = value;
                let _ = this.settings.save(&this.paths);
            }
        })
        .detach();

        cx.subscribe(&second_input, |this, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                if let Ok(value) = text.parse::<u32>() {
                    this.settings.researcher.second_researcher_threshold = value;
                    let _ = this.settings.save(&this.paths);
                }
            }
        })
        .detach();

        cx.subscribe_in(&second_input, window, |this, input, event, window, cx| {
            let NumberInputEvent::Step(action) = event;
            if let Some(value) = stepped_value(&input.read(cx).value(), *action) {
                input.update(cx, |input, cx| {
                    input.set_value(value.to_string(), window, cx);
                });
                this.settings.researcher.second_researcher_threshold = value;
                let _ = this.settings.save(&this.paths);
            }
        })
        .detach();

        view
    }
}

fn stepped_value(text: &str, action: StepAction) -> Option<u32> {
    let value = text.parse::<u32>().ok()?;
    Some(match action {
        StepAction::Increment => value.saturating_add(1),
        StepAction::Decrement => value.saturating_sub(1),
    })
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .size_full()
            .bg(theme.background)
            .p_4()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child("Interview settings"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Researcher queue thresholds"),
            )
            .child(threshold_row(
                cx,
                &self.replenish_input,
                "Replenish when open count below",
                "Start a researcher run when fewer than this many questions remain open.",
            ))
            .child(threshold_row(
                cx,
                &self.second_input,
                "Start second researcher when open count below",
                "While a researcher is already running, start another if open count drops below this threshold (max 2 concurrent).",
            ))
    }
}

fn threshold_row(
    cx: &App,
    input: &Entity<InputState>,
    label: impl Into<SharedString>,
    help: impl Into<SharedString>,
) -> impl IntoElement {
    let theme = cx.theme();
    let label = label.into();
    let help = help.into();

    h_flex()
        .gap_3()
        .items_start()
        .child(
            NumberInput::new(input)
                .appearance(true)
                .w(px(120.))
                .into_any_element(),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .whitespace_normal()
                        .child(help),
                ),
        )
}
