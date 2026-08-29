use crate::interview::TodPaths;
use crate::interview::settings::{MAX_LOG_MAX_SIZE_KB, MIN_LOG_MAX_SIZE_KB, TodSettings};
use crate::logging;
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
use gpui::{
    Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, Timer, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Selectable, StyledExt, h_flex, v_flex};
use std::time::Duration;

const SAVE_DEBOUNCE: Duration = Duration::from_secs(2);
const PANEL_WIDTH: f32 = 560.0;
const SIDEBAR_WIDTH: f32 = 168.0;

const SECTIONS: [SettingsSection; 3] = [
    SettingsSection::Researcher,
    SettingsSection::AnswerProcessor,
    SettingsSection::Logging,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsSection {
    Researcher,
    AnswerProcessor,
    Logging,
}

impl SettingsSection {
    fn label(self) -> &'static str {
        match self {
            Self::Researcher => "Researcher thresholds",
            Self::AnswerProcessor => "Answer-processor pool",
            Self::Logging => "Diagnostic logging",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Researcher => "researcher",
            Self::AnswerProcessor => "answer-processor",
            Self::Logging => "logging",
        }
    }
}

pub struct SettingsView {
    paths: TodPaths,
    settings: TodSettings,
    log_dir_display: SharedString,
    focus_handle: FocusHandle,
    app_nav: AppNavMenu,
    active_section: SettingsSection,
    save_generation: u64,
}

impl SettingsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let settings = TodSettings::load(&paths).expect("failed to load tod settings");
        let log_dir_display = SharedString::from(
            logging::absolute_log_dir(&paths.log_dir())
                .display()
                .to_string(),
        );

        Self {
            paths,
            settings,
            log_dir_display,
            focus_handle: cx.focus_handle(),
            app_nav: AppNavMenu::default(),
            active_section: SettingsSection::Researcher,
            save_generation: 0,
        }
    }

    fn activate_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        if self.active_section == section {
            return;
        }
        self.active_section = section;
        cx.notify();
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        let entity = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            Timer::after(SAVE_DEBOUNCE).await;
            let _ = entity.update(cx, |this, cx| {
                if this.save_generation == generation {
                    this.flush_save(cx);
                }
            });
        })
        .detach();
    }

    fn flush_save(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.settings.save(&self.paths) {
            tracing::error!("failed to save settings: {err:#}");
            return;
        }
        let _ = logging::reload_level(self.settings.log_level);
        let _ = logging::set_max_size_kb(self.settings.log_max_size_kb);
        cx.notify();
    }

    fn step_replenish(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.researcher.replenish_threshold =
            step_u32(self.settings.researcher.replenish_threshold, delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_second(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.researcher.second_researcher_threshold =
            step_u32(self.settings.researcher.second_researcher_threshold, delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_pool_size(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.answer_processor.session_pool_size =
            step_u32(self.settings.answer_processor.session_pool_size, delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_answers_per_session(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.answer_processor.answers_per_session =
            step_u32(self.settings.answer_processor.answers_per_session, delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_log_level(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.settings.log_level = self.settings.log_level.step(delta);
        self.schedule_save(cx);
        cx.notify();
    }

    fn step_log_max_size(&mut self, delta: i64, cx: &mut Context<Self>) {
        let next = if delta >= 0 {
            self.settings.log_max_size_kb.saturating_add(delta as u64)
        } else {
            self.settings
                .log_max_size_kb
                .saturating_sub((-delta) as u64)
        };
        self.settings.log_max_size_kb = TodSettings::clamp_log_max_size_kb(next);
        self.schedule_save(cx);
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

impl HasAppNav for SettingsView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        Some(AppDestination::Settings)
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let focus = self.focus_handle.clone();

        let root = v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.render_app_nav(window, cx)),
            )
            .child(
                v_flex().flex_1().min_h_0().overflow_hidden().p_4().child(
                    h_flex()
                        .w(px(PANEL_WIDTH))
                        .items_start()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .child(self.render_section_sidebar(cx, &theme))
                        .child(self.render_section_panel(cx, &theme)),
                ),
            )
            .track_focus(&focus);

        self.bind_app_nav_toggle(root, cx)
    }
}

impl SettingsView {
    fn render_section_sidebar(
        &self,
        cx: &mut Context<Self>,
        theme: &gpui_component::Theme,
    ) -> impl IntoElement {
        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .children(SECTIONS.iter().map(|section| {
                let selected = self.active_section == *section;
                Button::new(SharedString::from(format!(
                    "settings-section-{}",
                    section.id()
                )))
                .label(section.label())
                .ghost()
                .w_full()
                .selected(selected)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.activate_section(*section, cx);
                }))
            }))
    }

    fn render_section_panel(
        &self,
        cx: &mut Context<Self>,
        theme: &gpui_component::Theme,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_w(px(PANEL_WIDTH - SIDEBAR_WIDTH))
            .p_3()
            .gap_3()
            .child(self.render_active_section(cx, theme))
    }

    fn render_active_section(
        &self,
        cx: &mut Context<Self>,
        theme: &gpui_component::Theme,
    ) -> impl IntoElement {
        match self.active_section {
            SettingsSection::Researcher => v_flex()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child("Researcher thresholds"),
                )
                .child(threshold_row(
                    cx,
                    "replenish",
                    self.settings.researcher.replenish_threshold.to_string(),
                    "Replenish below",
                    "Start a researcher run when open questions fall under this count. Default 8.",
                    theme.foreground,
                    theme.muted_foreground,
                    |this, _, cx| this.step_replenish(-1, cx),
                    |this, _, cx| this.step_replenish(1, cx),
                ))
                .child(threshold_row(
                    cx,
                    "second",
                    self.settings
                        .researcher
                        .second_researcher_threshold
                        .to_string(),
                    "Second researcher below",
                    "While one researcher is already running, start a second if open count drops under this lower threshold. Max two runs. Default 2.",
                    theme.foreground,
                    theme.muted_foreground,
                    |this, _, cx| this.step_second(-1, cx),
                    |this, _, cx| this.step_second(1, cx),
                ))
                .into_any_element(),
            SettingsSection::AnswerProcessor => v_flex()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child("Answer-processor pool"),
                )
                .child(threshold_row(
                    cx,
                    "pool-size",
                    self.settings
                        .answer_processor
                        .session_pool_size
                        .to_string(),
                    "Maximum session pool size",
                    "Cap on concurrent open answer-processor sessions. Default 4.",
                    theme.foreground,
                    theme.muted_foreground,
                    |this, _, cx| this.step_pool_size(-1, cx),
                    |this, _, cx| this.step_pool_size(1, cx),
                ))
                .child(threshold_row(
                    cx,
                    "answers-per-session",
                    self.settings
                        .answer_processor
                        .answers_per_session
                        .to_string(),
                    "Answers per session",
                    "After the Nth response is processed on one session, close that session. Default 4.",
                    theme.foreground,
                    theme.muted_foreground,
                    |this, _, cx| this.step_answers_per_session(-1, cx),
                    |this, _, cx| this.step_answers_per_session(1, cx),
                ))
                .into_any_element(),
            SettingsSection::Logging => v_flex()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child("Diagnostic logging"),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(theme.foreground)
                                .child("Log directory"),
                        )
                        .child(
                            div()
                                .id("log-dir-path")
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .whitespace_normal()
                                .child(self.log_dir_display.clone()),
                        ),
                )
                .child(threshold_row(
                    cx,
                    "log-level",
                    self.settings.log_level.to_string(),
                    "Log verbosity",
                    "Minimum diagnostic log level (error, info, debug, trace). Default is info.",
                    theme.foreground,
                    theme.muted_foreground,
                    |this, _, cx| this.step_log_level(-1, cx),
                    |this, _, cx| this.step_log_level(1, cx),
                ))
                .child(threshold_row(
                    cx,
                    "log-max-size",
                    format!("{} KB", self.settings.log_max_size_kb),
                    "Max log storage (KB)",
                    format!(
                        "Maximum on-disk diagnostic log size in kilobytes ({MIN_LOG_MAX_SIZE_KB}–{MAX_LOG_MAX_SIZE_KB}). Default 51200 KB."
                    ),
                    theme.foreground,
                    theme.muted_foreground,
                    |this, _, cx| this.step_log_max_size(-1024, cx),
                    |this, _, cx| this.step_log_max_size(1024, cx),
                ))
                .into_any_element(),
        }
    }
}

fn threshold_row(
    cx: &mut Context<SettingsView>,
    id_prefix: &'static str,
    value: impl Into<SharedString>,
    label: impl Into<SharedString>,
    help: impl Into<SharedString>,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    on_dec: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
    on_inc: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let help = help.into();
    let value = value.into();

    h_flex()
        .w_full()
        .gap_3()
        .items_start()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .flex_shrink_0()
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
                        .w(px(56.))
                        .px_2()
                        .py_2()
                        .text_sm()
                        .font_semibold()
                        .text_color(foreground)
                        .child(value),
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
                .min_w(px(220.))
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
                        .text_sm()
                        .text_color(muted)
                        .whitespace_normal()
                        .child(help),
                ),
        )
}
