use super::always_on_top;
use super::fleet_blocked::FleetBlockedView;
use super::no_focus;
use crate::agent_socket::{self, LaunchOptions};
use crate::fleet::{FleetLaunchError, FleetStore};
use crate::interview::agent::SharedAgent;
use crate::interview::views::{SessionsEvent, SessionsView, SettingsView};
use crate::interview::{TaskListProceedContext, TodPaths, TodSettings};
use crate::process::{interview_phase_for_lifecycle, interview_phase_label};
use crate::ui::app_nav::{
    HasAppNav, ShellGoDatabase, ShellGoSettings, ShellGoTasks, register_app_nav_keyboard_bindings,
};
use crate::views::database::DatabaseView;
use crate::views::task_list::{TaskListEvent, TaskListView};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, IconName, Root, Selectable, StyledExt, TitleBar, h_flex};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellView {
    Tasks,
    Interview,
    Settings,
    Database,
}

struct PendingOpenInterview {
    task_id: String,
    entity_path: PathBuf,
    lifecycle: String,
    title: String,
}

struct PendingOpenLifecycle {
    task_id: String,
    lifecycle: String,
}

struct Shell {
    active_view: ShellView,
    task_list: Entity<TaskListView>,
    sessions: Entity<SessionsView>,
    settings: Entity<SettingsView>,
    database: Entity<DatabaseView>,
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    migration_notice_dismissed: bool,
    pending_open_interview: Option<PendingOpenInterview>,
    pending_open_lifecycle: Option<PendingOpenLifecycle>,
    pending_return_to_tasks: bool,
    always_on_top: bool,
    _task_list_subscription: Subscription,
    _sessions_subscription: Subscription,
}

impl Shell {
    fn select_view(&mut self, view: ShellView, window: &mut Window, cx: &mut Context<Self>) {
        self.task_list
            .update(cx, |list, _| list.app_nav_mut().close());
        self.sessions
            .update(cx, |sessions, cx| sessions.close_app_nav(cx));
        self.settings
            .update(cx, |settings, _| settings.app_nav_mut().close());
        self.database
            .update(cx, |database, _| database.app_nav_mut().close());
        if self.active_view == view {
            if view == ShellView::Tasks {
                self.task_list.update(cx, |list, cx| {
                    list.refresh(window, cx);
                });
            }
            return;
        }
        self.active_view = view;
        match view {
            ShellView::Tasks => {
                self.task_list.update(cx, |list, cx| {
                    list.refresh(window, cx);
                });
                let focus = self.task_list.read(cx).focus_handle(cx);
                focus.focus(window);
            }
            ShellView::Interview => {
                self.sessions.update(cx, |sessions, _| {
                    sessions.focus(window);
                });
            }
            ShellView::Settings => {
                let focus = self.settings.read(cx).focus_handle(cx);
                focus.focus(window);
            }
            ShellView::Database => {
                let focus = self.database.read(cx).focus_handle(cx);
                focus.focus(window);
            }
        }
        cx.notify();
    }

    fn queue_open_interview(
        &mut self,
        task_id: String,
        entity_path: PathBuf,
        lifecycle: String,
        title: String,
        cx: &mut Context<Self>,
    ) {
        self.pending_open_interview = Some(PendingOpenInterview {
            task_id,
            entity_path,
            lifecycle,
            title,
        });
        self.active_view = ShellView::Interview;
        cx.notify();
    }

    fn drain_pending_return_to_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_return_to_tasks {
            return;
        }
        self.pending_return_to_tasks = false;
        self.select_view(ShellView::Tasks, window, cx);
    }

    fn drain_pending_open_interview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_open_interview.take() else {
            return;
        };
        let phase = interview_phase_for_lifecycle(&pending.lifecycle)
            .unwrap_or("task-requirements-interview");
        let phase_label = interview_phase_label(phase);
        self.sessions.update(cx, |sessions, cx| {
            sessions.open_or_kickoff_for_entity(
                pending.entity_path,
                phase,
                &pending.title,
                phase_label,
                Some(TaskListProceedContext {
                    task_id: pending.task_id,
                    lifecycle: pending.lifecycle,
                }),
                window,
                cx,
            );
        });
    }

    fn drain_pending_open_lifecycle(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_open_lifecycle.take() else {
            return;
        };
        self.task_list.update(cx, |list, cx| {
            list.open_lifecycle_panel(&pending.task_id, &pending.lifecycle, cx);
        });
    }

    fn dismiss_migration_notice(&mut self, cx: &mut Context<Self>) {
        self.migration_notice_dismissed = true;
        cx.notify();
    }

    fn toggle_always_on_top(&mut self, cx: &mut Context<Self>) {
        let next = !self.always_on_top;
        if always_on_top::set(next) {
            self.always_on_top = next;
            self.persist_always_on_top(next);
            cx.notify();
        }
    }

    fn persist_always_on_top(&self, enabled: bool) {
        match TodSettings::load(&self.paths) {
            Ok(mut settings) => {
                settings.always_on_top = enabled;
                if let Err(err) = settings.save(&self.paths) {
                    tracing::error!("failed to save always_on_top setting: {err:#}");
                }
            }
            Err(err) => {
                tracing::error!("failed to load settings for always_on_top save: {err:#}");
            }
        }
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child("tod")
                .when(always_on_top::is_supported(), |bar| {
                    bar.child(
                        Button::new("always-on-top")
                            .ghost()
                            .compact()
                            .selected(self.always_on_top)
                            .icon(if self.always_on_top {
                                IconName::Star
                            } else {
                                IconName::StarOff
                            })
                            .tooltip(if self.always_on_top {
                                "Unpin window"
                            } else {
                                "Always on top"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_always_on_top(cx);
                            })),
                    )
                }),
        )
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_pending_open_interview(window, cx);
        self.drain_pending_return_to_tasks(window, cx);
        self.drain_pending_open_lifecycle(cx);

        div()
            .v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &ShellGoTasks, window, cx| {
                this.select_view(ShellView::Tasks, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellGoSettings, window, cx| {
                this.select_view(ShellView::Settings, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellGoDatabase, window, cx| {
                this.select_view(ShellView::Database, window, cx);
            }))
            .child(self.render_title_bar(cx))
            .child(self.render_migration_notice(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .w_full()
                    .child(self.render_content(window, cx)),
            )
    }
}

impl Shell {
    fn render_migration_notice(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.migration_notice_dismissed || !self.fleet.migration_in_progress() {
            return div().into_any_element();
        }
        let border = cx.theme().border;
        div()
            .v_flex()
            .gap_2()
            .px_4()
            .py_2()
            .bg(gpui::yellow())
            .text_color(gpui::black())
            .border_b_1()
            .border_color(border)
            .child("Storage-root migration is in progress. Fleet mutations remain blocked.")
            .child(
                gpui_component::button::Button::new("migration-notice-dismiss")
                    .label("Dismiss")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dismiss_migration_notice(cx);
                    })),
            )
            .into_any_element()
    }

    fn render_content(&self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        match self.active_view {
            ShellView::Tasks => self.task_list.clone().into_any_element(),
            ShellView::Interview => self.sessions.clone().into_any_element(),
            ShellView::Settings => self.settings.clone().into_any_element(),
            ShellView::Database => self.database.clone().into_any_element(),
        }
    }
}

fn resolve_fleet_root() -> Result<PathBuf, anyhow::Error> {
    let paths = TodPaths::discover()?;
    let settings = TodSettings::load(&paths)?;
    settings.resolve_fleet_storage_root()
}

fn open_fleet_store() -> Result<Arc<FleetStore>, (FleetLaunchError, PathBuf)> {
    let root = resolve_fleet_root()
        .map_err(|err| (FleetLaunchError::Other(err), PathBuf::from("<unresolved>")))?;
    FleetStore::open(&root)
        .map(Arc::new)
        .map_err(|err| (err, root))
}

pub fn open(cx: &mut AsyncApp, opts: LaunchOptions) -> Result<()> {
    // Eager-init interview persistence so config dir and defaults exist on first launch.
    let _ = crate::interview::bootstrap();

    let socket_addr = opts.agent_socket;
    let width = opts.width;
    let height = opts.height;
    let no_focus = opts.no_focus;
    let agent: SharedAgent;
    let bootstrap_gate: crate::interview::agent::BootstrapGate;
    {
        let (a, g) = opts.agent_backend.create();
        agent = a;
        bootstrap_gate = g;
    }

    let fleet_open = open_fleet_store();
    let paths = TodPaths::discover()?;
    let app_settings = TodSettings::load(&paths).unwrap_or_default();
    let restore_always_on_top = app_settings.always_on_top;

    #[cfg(windows)]
    let previous_foreground = if no_focus {
        no_focus::foreground_hwnd()
    } else {
        None
    };

    let handle = cx.open_window(
        WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(width), px(height)),
            })),
            is_resizable: socket_addr.is_none(),
            focus: !no_focus,
            ..Default::default()
        },
        {
            let paths = paths.clone();
            move |window, cx| match fleet_open {
                Err((error, resolved_root)) => {
                    let view = cx.new(|cx| FleetBlockedView::new(error, resolved_root, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                }
                Ok(fleet) => {
                    let task_list = cx.new(|cx| TaskListView::new(window, cx, fleet.clone()));
                    let agent_for_sessions = agent.clone();
                    let gate_for_sessions = bootstrap_gate.clone();
                    let sessions = cx.new(|cx| {
                        SessionsView::new(window, cx, agent_for_sessions, gate_for_sessions)
                    });
                    let settings = cx.new(|cx| SettingsView::new(window, cx));
                    let database = cx.new(|cx| DatabaseView::new(window, cx, fleet.clone()));
                    let view = cx.new(|cx| {
                        let _task_list_subscription = cx.subscribe(
                            &task_list,
                            |this: &mut Shell, _, event, cx| match event {
                                TaskListEvent::OpenInterview {
                                    task_id,
                                    entity_path,
                                    lifecycle,
                                    title,
                                } => {
                                    this.queue_open_interview(
                                        task_id.clone(),
                                        entity_path.clone(),
                                        lifecycle.clone(),
                                        title.clone(),
                                        cx,
                                    );
                                }
                                TaskListEvent::OpenTaskEdit { title, .. } => {
                                    eprintln!("tod: task edit stub — {title}");
                                }
                                TaskListEvent::OpenNewTaskCompose => {
                                    eprintln!("tod: new task compose stub");
                                }
                                TaskListEvent::CloseTaskEdit => {
                                    eprintln!("tod: close task edit stub");
                                }
                                TaskListEvent::OpenLifecycle { lifecycle, .. } => {
                                    eprintln!("tod: lifecycle panel stub — {lifecycle}");
                                }
                                TaskListEvent::OpenAgentDetail { task_id, agent_id } => {
                                    eprintln!(
                                        "tod: agent detail stub — task {task_id} agent {:?}",
                                        agent_id
                                    );
                                }
                                TaskListEvent::OpenShell {
                                    task_id,
                                    shell_id,
                                    agent_id,
                                } => {
                                    eprintln!(
                                        "tod: shell stub — task {task_id} shell {:?} agent {:?}",
                                        shell_id, agent_id
                                    );
                                }
                                TaskListEvent::DeleteTask { task_id } => {
                                    this.task_list.update(cx, |list, cx| {
                                        list.schedule_remove_task(task_id.clone(), cx);
                                    });
                                }
                                TaskListEvent::StatusMessage(msg) => {
                                    eprintln!("tod: {msg}");
                                }
                            },
                        );
                        let _sessions_subscription =
                            cx.subscribe(&sessions, |this: &mut Shell, _, event, cx| match event {
                                SessionsEvent::ReturnToTaskList => {
                                    this.pending_return_to_tasks = true;
                                    cx.notify();
                                }
                                SessionsEvent::ProceedToLifecycle { task_id, lifecycle } => {
                                    this.pending_open_lifecycle = Some(PendingOpenLifecycle {
                                        task_id: task_id.clone(),
                                        lifecycle: lifecycle.clone(),
                                    });
                                    this.pending_return_to_tasks = true;
                                    cx.notify();
                                }
                            });
                        Shell {
                            active_view: ShellView::Tasks,
                            task_list,
                            sessions,
                            settings,
                            database,
                            fleet: fleet.clone(),
                            paths,
                            migration_notice_dismissed: false,
                            pending_open_interview: None,
                            pending_open_lifecycle: None,
                            pending_return_to_tasks: false,
                            always_on_top: restore_always_on_top,
                            _task_list_subscription,
                            _sessions_subscription,
                        }
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                }
            }
        },
    )?;

    #[cfg(windows)]
    if no_focus {
        no_focus::after_window_open(previous_foreground);
    }

    if restore_always_on_top {
        always_on_top::set(true);
    }

    if let Some(addr) = socket_addr {
        agent_socket::start(cx, handle.into(), addr, width, height);
    }

    Ok(())
}

pub fn register_shell_keyboard_bindings(cx: &mut App) {
    register_app_nav_keyboard_bindings(cx);
}
