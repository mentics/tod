use gpui::{
    App, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{IconName, Root, Sizable, StyledExt, WindowExt, h_flex};
use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::selectable_text::selectable_text;

struct ConfirmToast;
struct ErrorBannerNotification;
struct WarningBannerNotification;
struct CloseGuardToast;

/// Overlay for queued notifications (error banners, confirm toasts).
pub fn notification_overlay(window: &mut Window, cx: &mut App) -> Option<impl IntoElement + use<>> {
    Root::render_notification_layer(window, cx)
}

/// Prominent red error banner in the top-right corner.
pub fn error_toast(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    let message = message.into();
    window.push_notification(
        Notification::new()
            .id::<ErrorBannerNotification>()
            .autohide(false)
            .bg(gpui::hsla(0., 0., 0., 0.))
            .border_0()
            .shadow_none()
            .p_0()
            .content(move |_note, window, cx| {
                h_flex()
                    .id("error-banner")
                    .max_w(px(480.))
                    .min_w(px(240.))
                    .px_4()
                    .py_2p5()
                    .gap_2()
                    .bg(gpui::red())
                    .rounded_lg()
                    .shadow_lg()
                    .items_start()
                    .child(
                        // Leave the top-right corner clear: the notification
                        // wrapper draws its own close button there on hover,
                        // so this banner must not add a second one.
                        div().flex_1().min_w_0().pr_5().child(
                            selectable_text("error-banner-text", message.clone(), window, cx)
                                .text_sm()
                                .text_color(gpui::white()),
                        ),
                    )
                    .into_any_element()
            }),
        cx,
    );
}

/// Amber banner for something the user should know that is not a failure.
pub fn warning_toast(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    let message = message.into();
    window.push_notification(
        Notification::new()
            .id::<WarningBannerNotification>()
            .autohide(false)
            .bg(gpui::hsla(0., 0., 0., 0.))
            .border_0()
            .shadow_none()
            .p_0()
            .content(move |_note, window, cx| {
                h_flex()
                    .id("warning-banner")
                    .max_w(px(480.))
                    .min_w(px(240.))
                    .px_4()
                    .py_2p5()
                    .gap_2()
                    .bg(gpui::rgb(0xb45309))
                    .rounded_lg()
                    .shadow_lg()
                    .items_start()
                    .child(
                        div().flex_1().min_w_0().pr_5().child(
                            selectable_text("warning-banner-text", message.clone(), window, cx)
                                .text_sm()
                                .text_color(gpui::white()),
                        ),
                    )
                    .into_any_element()
            }),
        cx,
    );
}

/// Standard yes/no confirmation toast (non-autohide, warning style).
pub fn confirm_toast(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    message: impl Into<SharedString>,
    on_yes: impl FnOnce(&mut Window, &mut App) + 'static,
    on_no: impl FnOnce(&mut Window, &mut App) + 'static,
) {
    let on_yes = Rc::new(RefCell::new(Some(on_yes)));
    let on_no = Rc::new(RefCell::new(Some(on_no)));

    let title = title.into();
    let message = message.into();

    window.push_notification(
        Notification::new()
            .icon(IconName::TriangleAlert)
            .autohide(false)
            .id::<ConfirmToast>()
            .content(move |_note, window, cx| {
                let on_yes = on_yes.clone();
                let on_no = on_no.clone();
                gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        selectable_text("confirm-toast-title", title.clone(), window, cx)
                            .text_sm()
                            .font_semibold(),
                    )
                    .child(selectable_text(
                        "confirm-toast-message",
                        message.clone(),
                        window,
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .mt_2()
                            .child(Button::new("toast-no").label("No").on_click(cx.listener(
                                move |note, _, window, cx| {
                                    note.dismiss(window, cx);
                                    if let Some(on_no) = on_no.borrow_mut().take() {
                                        on_no(window, cx);
                                    }
                                },
                            )))
                            .child(Button::new("toast-yes").label("Yes").primary().on_click(
                                cx.listener(move |note, _, window, cx| {
                                    note.dismiss(window, cx);
                                    if let Some(on_yes) = on_yes.borrow_mut().take() {
                                        on_yes(window, cx);
                                    }
                                }),
                            )),
                    )
                    .into_any_element()
            }),
        cx,
    );
}

/// Warns that background work (agent runs, gate checks, etc.) is still
/// active before letting the window close. `on_force_exit` performs the
/// actual close, bypassing whatever guard raised this toast.
pub fn close_guard_toast(
    window: &mut Window,
    cx: &mut App,
    running: Vec<SharedString>,
    on_force_exit: impl Fn(&mut Window, &mut App) + 'static,
) {
    let on_force_exit = Rc::new(on_force_exit);

    window.push_notification(
        Notification::new()
            .autohide(false)
            .id::<CloseGuardToast>()
            .bg(gpui::hsla(0., 0., 0., 0.))
            .border_0()
            .shadow_none()
            .p_0()
            .content(move |_note, window, cx| {
                let on_force_exit = on_force_exit.clone();
                gpui::div()
                    .id("close-guard-toast")
                    .max_w(px(360.))
                    .min_w(px(280.))
                    .px_4()
                    .py_3()
                    .gap_1()
                    .flex()
                    .flex_col()
                    // Dim reddish-gray so the warning reads clearly without
                    // the alarm of a full red banner.
                    .bg(gpui::rgb(0x3a2c2c))
                    .rounded_lg()
                    .shadow_lg()
                    // Interacting with anything else means the user isn't
                    // trying to exit after all — clear the warning instead
                    // of leaving it stuck on screen.
                    .on_mouse_down_out(|_, window, cx| {
                        window.remove_notification::<CloseGuardToast>(cx);
                    })
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                gpui_component::Icon::new(IconName::TriangleAlert)
                                    .text_color(gpui::rgb(0xf0a0a0))
                                    .small(),
                            )
                            .child(
                                selectable_text(
                                    "close-guard-title",
                                    "Background work is still running",
                                    window,
                                    cx,
                                )
                                .text_sm()
                                .font_semibold()
                                .text_color(gpui::white()),
                            ),
                    )
                    .child(gpui::div().flex().flex_col().gap_0p5().children(
                        running.iter().enumerate().map(|(i, item)| {
                            selectable_text(
                                SharedString::from(format!("close-guard-item-{i}")),
                                item.clone(),
                                window,
                                cx,
                            )
                            .text_xs()
                            .text_color(gpui::hsla(0., 0., 0.85, 1.))
                        }),
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .mt_2()
                            .child(Button::new("close-guard-cancel").label("Cancel").on_click(
                                cx.listener(|note, _, window, cx| {
                                    note.dismiss(window, cx);
                                }),
                            ))
                            .child(
                                Button::new("close-guard-force")
                                    .label("Exit anyway")
                                    .primary()
                                    .on_click(cx.listener(move |note, _, window, cx| {
                                        note.dismiss(window, cx);
                                        on_force_exit(window, cx);
                                    })),
                            ),
                    )
                    .into_any_element()
            }),
        cx,
    );
}
