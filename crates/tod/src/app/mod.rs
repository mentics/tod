mod always_on_top;
mod data_root_setup;
mod fleet_blocked;
mod history_window;
mod interactive_agent_window;
mod no_focus;
pub mod transcript_window;
pub mod window;

use crate::cli::LaunchOptions;
use crate::interview::views::register_sessions_keyboard_bindings;
use crate::ui::list::register_list_keyboard_bindings;
use crate::views::agent_config_panel::register_agent_config_keyboard_bindings;
use crate::views::agent_transcripts::register_agent_transcripts_keyboard_bindings;
use crate::views::command_history::register_command_history_keyboard_bindings;
use crate::views::interactive_agent::register_interactive_agent_keyboard_bindings;
use crate::views::obligations::register_obligations_keyboard_bindings;
use crate::views::task_edit::register_task_edit_keyboard_bindings;
use crate::views::task_list::register_task_list_keyboard_bindings;
use gpui::*;

pub use history_window::HistoryWindowControl;
pub use interactive_agent_window::{InteractiveAgentOpenParams, InteractiveAgentWindowControl};
pub use transcript_window::TranscriptWindowControl;
pub use window::{open, open_data_root_setup};

pub fn register_main_keyboard_bindings(cx: &mut gpui::App) {
    register_list_keyboard_bindings(cx);
    register_task_list_keyboard_bindings(cx);
    register_command_history_keyboard_bindings(cx);
    register_task_edit_keyboard_bindings(cx);
    register_obligations_keyboard_bindings(cx);
    register_agent_config_keyboard_bindings(cx);
    register_sessions_keyboard_bindings(cx);
    register_agent_transcripts_keyboard_bindings(cx);
    register_interactive_agent_keyboard_bindings(cx);
    window::register_shell_keyboard_bindings(cx);
}

pub(crate) fn launch_main_application(
    opts: LaunchOptions,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    crate::init_logging(&opts)?;
    cx.update(|app| register_main_keyboard_bindings(app));
    open(cx, opts)
}

pub struct App;

impl App {
    pub fn run(opts: LaunchOptions, needs_data_root_setup: bool) {
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
            if !needs_data_root_setup {
                register_main_keyboard_bindings(cx);
            }

            cx.spawn(async move |cx| {
                let result = if needs_data_root_setup {
                    open_data_root_setup(cx, opts)
                } else if let Err(err) = open(cx, opts) {
                    Err(err)
                } else {
                    Ok(())
                };
                if let Err(err) = result {
                    eprintln!("tod: {err:#}");
                    std::process::exit(1);
                }
                Ok::<_, anyhow::Error>(())
            })
            .detach();
        });
    }
}
