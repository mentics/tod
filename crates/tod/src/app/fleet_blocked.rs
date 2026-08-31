//! Minimal UI when fleet storage blocks launch (user.md reqs 12–14).

use crate::fleet::FleetLaunchError;
use crate::interview::TodPaths;
use crate::interview::settings::TodSettings;
use crate::ui::selectable_text::selectable_text;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, StyledExt, v_flex};
use std::path::PathBuf;

/// Blocked-launch surface: persistent notice + fleet storage root settings.
pub struct FleetBlockedView {
    error_message: SharedString,
    resolved_root: PathBuf,
    paths: TodPaths,
    settings: TodSettings,
    root_input: Entity<InputState>,
    notice_dismissed: bool,
    status_line: SharedString,
    focus_handle: FocusHandle,
    _input_subscription: gpui::Subscription,
}

impl FleetBlockedView {
    pub fn new(
        error: FleetLaunchError,
        resolved_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let settings = TodSettings::load(&paths).expect("failed to load tod settings");
        let root_display = resolved_root.display().to_string();
        let root_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(root_display)
                .placeholder("Fleet storage root path")
        });
        let _input_subscription = cx.subscribe(&root_input, |this, input, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.save_root(input.read(cx).value().to_string(), cx);
            }
        });

        Self {
            error_message: SharedString::from(error.to_string()),
            resolved_root,
            paths,
            settings,
            root_input,
            notice_dismissed: false,
            status_line: SharedString::default(),
            focus_handle: cx.focus_handle(),
            _input_subscription,
        }
    }

    fn save_root(&mut self, raw: String, cx: &mut Context<Self>) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            self.status_line = SharedString::from("Enter a storage root path.");
            cx.notify();
            return;
        }
        let path = PathBuf::from(trimmed);
        match crate::fleet::paths::normalize_absolute(&path) {
            Ok(normalized) => {
                self.settings.fleet_storage_root = Some(normalized);
            }
            Err(err) => {
                self.status_line = SharedString::from(format!("Invalid path: {err:#}"));
                cx.notify();
                return;
            }
        }
        if let Err(err) = self.settings.save(&self.paths) {
            self.status_line = SharedString::from(format!("Failed to save settings: {err:#}"));
            cx.notify();
            return;
        }
        self.status_line =
            SharedString::from("Storage root saved. Restart tod to apply the new path.");
        cx.notify();
    }

    fn dismiss_notice(&mut self, cx: &mut Context<Self>) {
        self.notice_dismissed = true;
        cx.notify();
    }
}

impl Focusable for FleetBlockedView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FleetBlockedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;

        v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(border)
                    .text_lg()
                    .font_semibold()
                    .text_color(foreground)
                    .child("tod — fleet storage blocked"),
            )
            .when(!self.notice_dismissed, |el| {
                el.child(
                    v_flex()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .bg(gpui::red())
                        .text_color(gpui::white())
                        .border_b_1()
                        .border_color(border)
                        .child(
                            selectable_text("fleet-blocked-error", self.error_message.clone(), window, cx)
                                .text_sm()
                                .text_color(gpui::white())
                                .whitespace_normal(),
                        )
                        .child(
                            Button::new("fleet-blocked-dismiss")
                                .label("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dismiss_notice(cx);
                                })),
                        ),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .p_4()
                    .gap_4()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(foreground)
                            .child("Fleet storage root"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .whitespace_normal()
                            .child(
                                "Launch is blocked until the storage root is valid and writable. \
                                 Back up the root only while no tod instance is running against it.",
                            ),
                    )
                    .child(
                        selectable_text(
                            "fleet-blocked-resolved-path",
                            format!("Current resolved path: {}", self.resolved_root.display()),
                            window,
                            cx,
                        )
                        .text_xs()
                        .text_color(muted)
                        .whitespace_normal(),
                    )
                    .child(Input::new(&self.root_input).w_full())
                    .child(
                        Button::new("fleet-blocked-save")
                            .label("Save storage root")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let value = this.root_input.read(cx).value().to_string();
                                this.save_root(value, cx);
                            })),
                    )
                    .when(!self.status_line.is_empty(), |el| {
                        el.child(
                            selectable_text("fleet-blocked-status", self.status_line.clone(), window, cx)
                                .text_sm()
                                .text_color(foreground)
                                .whitespace_normal(),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("After changing the path, restart tod before launch can proceed."),
                    ),
            )
    }
}
