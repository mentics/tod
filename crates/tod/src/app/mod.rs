mod window;

use crate::agent_socket::LaunchOptions;
use crate::interview::views::register_sessions_keyboard_bindings;
use crate::ui::list::register_list_keyboard_bindings;
use gpui::*;

pub use window::open;

pub struct App;

impl App {
    pub fn run(opts: LaunchOptions) {
        let app = Application::new().with_assets(gpui_component_assets::Assets);

        app.run(move |cx| {
            gpui_component::init(cx);
            register_list_keyboard_bindings(cx);
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
