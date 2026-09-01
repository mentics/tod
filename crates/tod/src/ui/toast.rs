use gpui::{
    App, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{IconName, Root, Sizable, WindowExt, h_flex};
use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::selectable_text::selectable_text;

struct ConfirmToast;
struct ErrorBannerNotification;

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
            .autohide(true)
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
                        div().flex_1().min_w_0().child(
                            selectable_text("error-banner-text", message.clone(), window, cx)
                                .text_sm()
                                .text_color(gpui::white()),
                        ),
                    )
                    .child(
                        Button::new("error-banner-dismiss")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .text_color(gpui::white())
                            .on_click(cx.listener(|note, _, window, cx| {
                                note.dismiss(window, cx);
                            })),
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

    window.push_notification(
        Notification::warning(message)
            .title(title)
            .autohide(false)
            .id::<ConfirmToast>()
            .content(move |_note, _window, cx| {
                let on_yes = on_yes.clone();
                let on_no = on_no.clone();
                h_flex()
                    .gap_2()
                    .mt_2()
                    .child(Button::new("toast-no").label("No").on_click(cx.listener(
                        move |note, _, window, cx| {
                            note.dismiss(window, cx);
                            window.remove_notification::<ConfirmToast>(cx);
                            if let Some(on_no) = on_no.borrow_mut().take() {
                                on_no(window, cx);
                            }
                        },
                    )))
                    .child(
                        Button::new("toast-yes")
                            .label("Yes")
                            .primary()
                            .on_click(cx.listener(move |note, _, window, cx| {
                                note.dismiss(window, cx);
                                window.remove_notification::<ConfirmToast>(cx);
                                if let Some(on_yes) = on_yes.borrow_mut().take() {
                                    on_yes(window, cx);
                                }
                            })),
                    )
                    .into_any_element()
            }),
        cx,
    );
}
