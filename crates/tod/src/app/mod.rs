mod always_on_top;
mod fleet_blocked;
mod no_focus;
mod window;

use crate::agent_socket::LaunchOptions;
use crate::interview::views::register_sessions_keyboard_bindings;
use crate::ui::list::register_list_keyboard_bindings;
use crate::views::task_list::register_task_list_keyboard_bindings;
use gpui::*;

pub use window::open;

pub struct App;

impl App {
    pub fn run(opts: LaunchOptions) {
        let app = Application::new().with_assets(gpui_component_assets::Assets);

        app.run(move |cx| {
            gpui_component::init(cx);
            // Dark default accent (#171717) is nearly invisible for PopupMenu /
            // ListItem hover-selected chrome on #0a0a0a. Align with list_active
            // border so menu ↑/↓ selection is clearly visible (req 20).
            {
                use gpui_component::Theme;
                let theme = Theme::global_mut(cx);
                let border = theme.list_active_border;
                theme.accent = border.opacity(0.55);
                theme.accent_foreground = theme.foreground;
            }
            register_list_keyboard_bindings(cx);
            register_task_list_keyboard_bindings(cx);
            register_sessions_keyboard_bindings(cx);
            window::register_shell_keyboard_bindings(cx);

            cx.spawn(async move |cx| {
                window::open(cx, opts)?;
                Ok::<_, anyhow::Error>(())
            })
            .detach();
        });
    }
}
