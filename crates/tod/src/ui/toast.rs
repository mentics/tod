use gpui::{App, IntoElement, ParentElement, SharedString, Styled, Window};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;
use gpui_component::{WindowExt, h_flex};
use std::cell::RefCell;
use std::rc::Rc;

struct ConfirmToast;

/// Brief error toast for invalid submit attempts.
pub fn error_toast(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message).autohide(true), cx);
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
