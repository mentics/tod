use crate::interview::{TodPaths, TodSettings};
use gpui::{
    App, AppContext, Context, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};

pub struct SettingsView {
    paths: TodPaths,
    settings: TodSettings,
    focus_handle: FocusHandle,
}

impl SettingsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let settings = TodSettings::load(&paths).expect("failed to load tod settings");
        Self {
            paths,
            settings,
            focus_handle: cx.focus_handle(),
        }
    }

    fn step_replenish(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.researcher.replenish_threshold =
            step_u32(self.settings.researcher.replenish_threshold, delta);
        let _ = self.settings.save(&self.paths);
        cx.notify();
    }

    fn step_second(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.researcher.second_researcher_threshold =
            step_u32(self.settings.researcher.second_researcher_threshold, delta);
        let _ = self.settings.save(&self.paths);
        cx.notify();
    }
}

fn step_u32(value: u32, delta: i32) -> u32 {
    if delta >= 0 {
        value.saturating_add(delta as u32)
    } else {
        value.saturating_sub((-delta) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::step_u32;

    #[test]
    fn step_increments_and_decrements() {
        assert_eq!(step_u32(8, 1), 9);
        assert_eq!(step_u32(8, -1), 7);
        assert_eq!(step_u32(0, -1), 0);
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

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
                "replenish",
                self.settings.researcher.replenish_threshold,
                "Replenish when open count below",
                "Start a researcher run when fewer than this many questions remain open.",
                theme.foreground,
                theme.muted_foreground,
                |this, _, cx| this.step_replenish(-1, cx),
                |this, _, cx| this.step_replenish(1, cx),
            ))
            .child(threshold_row(
                cx,
                "second",
                self.settings.researcher.second_researcher_threshold,
                "Start second researcher when open count below",
                "While a researcher is already running, start another if open count drops below this threshold (max 2 concurrent).",
                theme.foreground,
                theme.muted_foreground,
                |this, _, cx| this.step_second(-1, cx),
                |this, _, cx| this.step_second(1, cx),
            ))
    }
}

fn threshold_row(
    cx: &mut Context<SettingsView>,
    id_prefix: &'static str,
    value: u32,
    label: impl Into<SharedString>,
    help: impl Into<SharedString>,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    on_dec: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
    on_inc: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let help = help.into();

    h_flex()
        .gap_3()
        .items_start()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new(SharedString::from(format!("{id_prefix}-dec")))
                        .label("−")
                        .w(px(48.))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            on_dec(this, window, cx);
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{id_prefix}-value")))
                        .min_w(px(48.))
                        .px_2()
                        .py_2()
                        .text_sm()
                        .font_semibold()
                        .text_color(foreground)
                        .child(value.to_string()),
                )
                .child(
                    Button::new(SharedString::from(format!("{id_prefix}-inc")))
                        .label("+")
                        .w(px(48.))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            on_inc(this, window, cx);
                        })),
                ),
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
                        .text_color(foreground)
                        .child(label),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_sm()
                        .text_color(muted)
                        .whitespace_normal()
                        .child(help),
                ),
        )
}
