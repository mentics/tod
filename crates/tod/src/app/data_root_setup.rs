//! First-launch UI: choose a data root and write `install.toml`.

use super::launch_main_application;
use crate::cli::LaunchOptions;
use crate::install::{default_data_root, save_data_root};
use crate::interview::set_data_root;
use crate::ui::selectable_text::selectable_text;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::path::PathBuf;

/// First-run surface: pick where durable tod state lives.
pub struct DataRootSetupView {
    opts: LaunchOptions,
    root_input: Entity<InputState>,
    status_line: SharedString,
    focus_handle: FocusHandle,
    input_focused: bool,
    pending_continue: bool,
    _input_subscription: gpui::Subscription,
}

impl DataRootSetupView {
    pub fn new(opts: LaunchOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let default_path = default_data_root()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|err| {
                tracing::warn!("failed to resolve default data root: {err:#}");
                String::new()
            });
        let root_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_path)
                .placeholder("Data root directory")
        });
        let _input_subscription = cx.subscribe(&root_input, |this, input, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let value = input.read(cx).value().to_string();
                this.stage_continue(value, cx);
            }
        });

        Self {
            opts,
            root_input,
            status_line: SharedString::default(),
            focus_handle: cx.focus_handle(),
            input_focused: false,
            pending_continue: false,
            _input_subscription,
        }
    }

    fn cancel(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }

    fn stage_continue(&mut self, _raw: String, cx: &mut Context<Self>) {
        if self.pending_continue {
            return;
        }
        self.pending_continue = true;
        cx.notify();
    }

    fn continue_with(&mut self, raw: String, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_continue = false;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            self.status_line = SharedString::from("Enter a data root path.");
            cx.notify();
            return;
        }
        let path = PathBuf::from(trimmed);
        let normalized = match tod_store::fleet::paths::normalize_absolute(&path) {
            Ok(path) => path,
            Err(err) => {
                self.status_line = SharedString::from(format!("Invalid path: {err:#}"));
                cx.notify();
                return;
            }
        };
        if let Err(err) = std::fs::create_dir_all(&normalized) {
            self.status_line = SharedString::from(format!("Failed to create directory: {err:#}"));
            cx.notify();
            return;
        }
        if let Err(err) = save_data_root(&normalized) {
            self.status_line =
                SharedString::from(format!("Failed to save install config: {err:#}"));
            cx.notify();
            return;
        }
        set_data_root(normalized);

        let opts = self.opts.clone();
        window.remove_window();
        cx.spawn(async move |_, cx| {
            if let Err(err) = launch_main_application(opts, cx) {
                eprintln!("tod: {err:#}");
                std::process::exit(1);
            }
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    }
}

impl Focusable for DataRootSetupView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DataRootSetupView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.input_focused {
            self.input_focused = true;
            self.root_input.read(cx).focus_handle(cx).focus(window);
        }
        if self.pending_continue {
            self.pending_continue = false;
            let value = self.root_input.read(cx).value().to_string();
            self.continue_with(value, window, cx);
        }

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
                    .child("tod — choose data location"),
            )
            .child(
                v_flex()
                    .flex_1()
                    .p_4()
                    .gap_4()
                    .child(div().text_sm().text_color(muted).whitespace_normal().child(
                        "Choose where tod stores your tasks, interviews, and settings. \
                                 This location is remembered for future launches.",
                    ))
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(foreground)
                            .child("Data root"),
                    )
                    .child(Input::new(&self.root_input).w_full())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("data-root-continue")
                                    .label("Continue")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let value = this.root_input.read(cx).value().to_string();
                                        this.continue_with(value, window, cx);
                                    })),
                            )
                            .child(Button::new("data-root-cancel").label("Cancel").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.cancel(window, cx);
                                }),
                            )),
                    )
                    .when(!self.status_line.is_empty(), |el| {
                        el.child(
                            selectable_text(
                                "data-root-status",
                                self.status_line.clone(),
                                window,
                                cx,
                            )
                            .text_sm()
                            .text_color(foreground)
                            .whitespace_normal(),
                        )
                    }),
            )
    }
}
