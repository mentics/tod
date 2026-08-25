use crate::agent_socket::{self, LaunchOptions};
use crate::interview::views::sessions::styled_tab;
use crate::interview::views::{SessionsView, SettingsView};
use crate::views::task_list::TaskListView;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{ActiveTheme, Root, StyledExt, TitleBar};

actions!(shell, [ShellTabTasks, ShellTabInterview, ShellTabSettings]);

pub fn register_shell_keyboard_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-1", ShellTabTasks, None),
        KeyBinding::new("ctrl-2", ShellTabInterview, None),
        KeyBinding::new("ctrl-3", ShellTabSettings, None),
    ]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellTab {
    Tasks,
    Interview,
    Settings,
}

struct Shell {
    active_tab: ShellTab,
    task_list: Entity<TaskListView>,
    sessions: Entity<SessionsView>,
    settings: Entity<SettingsView>,
}

impl Shell {
    fn select_tab(&mut self, tab: ShellTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab != tab {
            self.active_tab = tab;
            if tab == ShellTab::Interview {
                self.sessions.update(cx, |sessions, _| {
                    sessions.focus(window);
                });
            }
            cx.notify();
        }
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &ShellTabTasks, window, cx| {
                this.select_tab(ShellTab::Tasks, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellTabInterview, window, cx| {
                this.select_tab(ShellTab::Interview, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellTabSettings, window, cx| {
                this.select_tab(ShellTab::Settings, window, cx);
            }))
            .child(TitleBar::new().child("tod"))
            .child(self.render_tab_bar(cx))
            .child(div().flex_1().min_w_0().min_h_0().overflow_hidden().w_full().child(self.render_content(window, cx)))
    }
}

impl Shell {
    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(tab_button(
                cx,
                "Tasks (Ctrl+1)",
                self.active_tab == ShellTab::Tasks,
                |this, _, window, cx| this.select_tab(ShellTab::Tasks, window, cx),
            ))
            .child(tab_button(
                cx,
                "Interview (Ctrl+2)",
                self.active_tab == ShellTab::Interview,
                |this, _, window, cx| this.select_tab(ShellTab::Interview, window, cx),
            ))
            .child(tab_button(
                cx,
                "Settings (Ctrl+3)",
                self.active_tab == ShellTab::Settings,
                |this, _, window, cx| this.select_tab(ShellTab::Settings, window, cx),
            ))
    }

    fn render_content(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.active_tab {
            ShellTab::Tasks => self.task_list.clone().into_any_element(),
            ShellTab::Interview => self.sessions.clone().into_any_element(),
            ShellTab::Settings => self.settings.clone().into_any_element(),
        }
    }
}

fn tab_button(
    cx: &mut Context<Shell>,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&mut Shell, &ClickEvent, &mut Window, &mut Context<Shell>) + 'static,
) -> impl IntoElement {
    styled_tab(cx, label, active, on_click)
}

pub fn open(cx: &mut AsyncApp, opts: LaunchOptions) -> Result<()> {
    // Eager-init interview persistence so config dir and defaults exist on first launch.
    let _ = crate::interview::bootstrap();

    let socket_addr = opts.agent_socket;
    let width = opts.width;
    let height = opts.height;

    let handle = cx.open_window(
        WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(width), px(height)),
            })),
            is_resizable: socket_addr.is_none(),
            ..Default::default()
        },
        |window, cx| {
            let task_list = cx.new(|cx| TaskListView::new(window, cx));
            let sessions = cx.new(|cx| SessionsView::new(window, cx));
            let settings = cx.new(|cx| SettingsView::new(window, cx));
            let view = cx.new(|_| Shell {
                active_tab: ShellTab::Tasks,
                task_list,
                sessions,
                settings,
            });
            cx.new(|cx| Root::new(view, window, cx))
        },
    )?;

    if let Some(addr) = socket_addr {
        agent_socket::start(cx, handle.into(), addr, width, height);
    }

    Ok(())
}
