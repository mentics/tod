use gpui::{
    Action, App, IntoElement, ParentElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::ActiveTheme;
use gpui_component::StyledExt;
use gpui_component::kbd::Kbd;

struct ShortcutBadgeStyle {
    bg: gpui::Hsla,
    fg: gpui::Hsla,
    border: gpui::Hsla,
}

fn badge_style(cx: &App) -> ShortcutBadgeStyle {
    let theme = cx.theme();
    ShortcutBadgeStyle {
        bg: theme.secondary,
        fg: theme.foreground,
        border: theme.muted_foreground.opacity(0.55),
    }
}

fn shortcut_pill(kbd: Kbd, style: ShortcutBadgeStyle) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .px_1p5()
        .py_0p5()
        .rounded_sm()
        .border_1()
        .border_color(style.border)
        .bg(style.bg)
        .text_color(style.fg)
        .text_xs()
        .font_medium()
        .child(kbd.appearance(false))
}

/// High-contrast shortcut pill for suffix slots (search field, etc.).
pub fn render_shortcut_pill(
    window: &Window,
    action: &dyn Action,
    context: &str,
    cx: &App,
) -> Option<impl IntoElement> {
    render_shortcut_pill_in_context(window, action, Some(context), cx)
}

pub fn render_shortcut_pill_in_context(
    window: &Window,
    action: &dyn Action,
    context: Option<&str>,
    cx: &App,
) -> Option<impl IntoElement> {
    let kbd = Kbd::binding_for_action(action, context, window)?;
    Some(shortcut_pill(kbd, badge_style(cx)))
}

/// Actionable control with a readable lower-right shortcut badge and room to render it.
pub fn chrome_control_with_shortcut(
    control: impl IntoElement,
    window: &Window,
    action: &dyn Action,
    context: &str,
    cx: &App,
) -> impl IntoElement {
    chrome_control_with_shortcut_in_context(control, window, action, Some(context), cx)
}

pub fn chrome_control_with_shortcut_in_context(
    control: impl IntoElement,
    window: &Window,
    action: &dyn Action,
    context: Option<&str>,
    cx: &App,
) -> impl IntoElement {
    div()
        .relative()
        .flex_shrink_0()
        .pb(px(14.))
        .pr(px(2.))
        .child(control)
        .when_some(
            render_shortcut_pill_in_context(window, action, context, cx),
            |el, pill| el.child(div().absolute().bottom_0().right_0().child(pill)),
        )
}
