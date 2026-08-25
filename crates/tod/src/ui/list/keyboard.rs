use gpui::{App, KeyBinding, NoAction, actions};

actions!(
    list_nav,
    [
        ListPageUp,
        ListPageDown,
        ListHome,
        ListEnd,
        ListArrowUp,
        ListArrowDown
    ]
);

pub fn register_list_keyboard_bindings(cx: &mut App) {
    let context = Some("List");
    // gpui-component List binds ↑/↓ to wrap-around SelectUp/SelectDown. NoAction
    // shadows those; the clamped ListArrow* bindings must be registered after so
    // they still win (keymap: later binding first, NoAction stops older matches).
    cx.bind_keys([
        KeyBinding::new("up", NoAction, context),
        KeyBinding::new("down", NoAction, context),
    ]);
    cx.bind_keys([
        KeyBinding::new("up", ListArrowUp, context),
        KeyBinding::new("down", ListArrowDown, context),
        KeyBinding::new("pageup", ListPageUp, context),
        KeyBinding::new("pagedown", ListPageDown, context),
        KeyBinding::new("home", ListHome, context),
        KeyBinding::new("end", ListEnd, context),
    ]);
}
