//! The Files capability's "Runs in" section: this machine, or a running dev
//! container the user picks from `docker ps` (or names by hand).
//!
//! The repository usually lives in the container, and the workspace
//! directory is its path there. "Repository" switches to one on this machine
//! mounted into the container; the directory in the container then follows
//! from its mounts.
//!
//! Docker is only ever called off the UI thread. The container name
//! autosaves after a short pause in typing (Enter saves at once); choosing a
//! listed container saves immediately.

use super::{TaskEditField, TaskEditView, input_text};
use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    AppContext, Context, ElementId, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, div,
};
use gpui_component::button::Button;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{ActiveTheme, Disableable, h_flex, v_flex};
use std::time::Duration;
use tod_agent::devcontainer::{self, ContainerSummary};
use tod_store::fleet::{DevContainerSetting, FleetMutation};

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

pub(super) struct DevContainerPanel {
    pub(super) container_input: Entity<InputState>,
    /// Running containers from the last `docker ps`, dev containers first.
    containers: Vec<ContainerSummary>,
    listing: bool,
    list_error: Option<String>,
    /// Where launches would run, or why they cannot, per the last check.
    check: Option<Result<String, String>>,
    checking: bool,
    /// Bumped per node load and per check, so a late result for a node the
    /// panel has left is dropped.
    generation: u64,
    _save_task: Option<Task<()>>,
    _subscriptions: [Subscription; 1],
}

impl DevContainerPanel {
    pub(super) fn new(window: &mut Window, cx: &mut Context<TaskEditView>) -> Self {
        let container_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Enter to edit · Container name or ID")
        });
        let subscribe = |input: &Entity<InputState>, cx: &mut Context<TaskEditView>| {
            cx.subscribe(input, |this: &mut TaskEditView, _, event: &InputEvent, cx| {
                match event {
                    InputEvent::Change => this.schedule_dev_container_save(cx),
                    InputEvent::PressEnter { .. } => this.save_dev_container_inputs(cx),
                    _ => {}
                }
            })
        };
        let _subscriptions = [subscribe(&container_input, cx)];
        Self {
            container_input,
            containers: Vec::new(),
            listing: false,
            list_error: None,
            check: None,
            checking: false,
            generation: 0,
            _save_task: None,
            _subscriptions,
        }
    }

    pub(super) fn container_count(&self) -> usize {
        self.containers.len()
    }
}

impl TaskEditView {
    /// This node's own dev container setting; `None` runs on this machine.
    pub(super) fn own_dev_container(&self) -> Option<DevContainerSetting> {
        self.own_files().and_then(|files| files.dev_container.clone())
    }

    /// Put the node's setting in the inputs and, when it uses a dev
    /// container, list the running ones and check the chosen one.
    pub(super) fn load_dev_container(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dev._save_task = None;
        self.dev.generation += 1;
        self.dev.check = None;
        self.dev.checking = false;
        let dev = self.own_dev_container().unwrap_or_default();
        let container = dev.container().unwrap_or_default().to_string();
        self.dev.container_input.update(cx, |input, cx| {
            input.set_value(container, window, cx);
        });
        if self.own_dev_container().is_some() {
            if self.dev.containers.is_empty() && !self.dev.listing {
                self.refresh_containers(cx);
            }
            self.check_dev_container(cx);
        }
    }

    /// "Runs in": this machine ↔ dev container.
    pub(super) fn toggle_dev_container(&mut self, cx: &mut Context<Self>) {
        if self.own_files().is_none() {
            return;
        }
        let next = match self.own_dev_container() {
            Some(_) => None,
            None => Some(DevContainerSetting {
                container: Some(input_text(&self.dev.container_input, cx))
                    .filter(|c| !c.trim().is_empty()),
                repo_on_host: false,
            }),
        };
        let turning_on = next.is_some();
        if self.save_dev_container(next, cx) && turning_on {
            self.refresh_containers(cx);
        }
    }

    /// "Repository": inside the container ↔ on this machine, mounted.
    pub(super) fn toggle_repo_location(&mut self, cx: &mut Context<Self>) {
        let Some(dev) = self.own_dev_container() else {
            return;
        };
        self.save_dev_container(
            Some(DevContainerSetting {
                repo_on_host: !dev.repo_on_host,
                ..dev
            }),
            cx,
        );
    }

    /// Whether this node's own repository lives in its dev container.
    pub(super) fn repo_in_container(&self) -> bool {
        self.own_dev_container().is_some_and(|dev| !dev.repo_on_host)
    }

    fn schedule_dev_container_save(&mut self, cx: &mut Context<Self>) {
        if self.own_dev_container().is_none() {
            return;
        }
        // Replacing the task cancels the previous debounce.
        self.dev._save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.save_dev_container_inputs(cx));
        }));
    }

    fn save_dev_container_inputs(&mut self, cx: &mut Context<Self>) {
        self.dev._save_task = None;
        if self.own_dev_container().is_none() {
            return;
        }
        let container = input_text(&self.dev.container_input, cx).trim().to_string();
        if !container.is_empty()
            && let Err(err) = devcontainer::validate_container_ref(&container)
        {
            self.dev.check = Some(Err(format!("{err:#}")));
            cx.notify();
            return;
        }
        let repo_on_host = self.own_dev_container().is_some_and(|dev| dev.repo_on_host);
        self.save_dev_container(
            Some(DevContainerSetting {
                container: (!container.is_empty()).then_some(container),
                repo_on_host,
            }),
            cx,
        );
    }

    /// Choose a listed container.
    pub(super) fn choose_container(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chosen) = self.dev.containers.get(index) else {
            return;
        };
        let name = chosen.name.clone();
        self.dev.container_input.update(cx, |input, cx| {
            input.set_value(name.clone(), window, cx);
        });
        let current = self.own_dev_container().unwrap_or_default();
        self.save_dev_container(
            Some(DevContainerSetting {
                container: Some(name),
                ..current
            }),
            cx,
        );
    }

    /// Persist `setting` if it differs from the stored one. Returns whether
    /// the stored setting is now `setting`.
    fn save_dev_container(
        &mut self,
        setting: Option<DevContainerSetting>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(node_id) = self.task_id() else {
            return false;
        };
        let normalize = |dev: Option<DevContainerSetting>| {
            dev.map(|dev| DevContainerSetting {
                container: dev.container().map(str::to_string),
                repo_on_host: dev.repo_on_host,
            })
        };
        let setting = normalize(setting);
        if normalize(self.own_dev_container()) == setting {
            return true;
        }
        if let Err(err) = self.fleet.enqueue(FleetMutation::SetNodeDevContainer {
            node_id,
            dev_container: setting,
        }) {
            self.pending_toast = Some(format!("Failed to save where the node runs: {err}"));
            cx.notify();
            return false;
        }
        let _ = self.fleet.writer().flush();
        let _ = self.fleet.reload_if_stale();
        self.load_action_capabilities();
        self.clamp_focus_index();
        self.check_dev_container(cx);
        self.notify_changed(cx);
        true
    }

    /// `docker ps`, off the UI thread.
    pub(super) fn refresh_containers(&mut self, cx: &mut Context<Self>) {
        if self.dev.listing {
            return;
        }
        self.dev.listing = true;
        self.dev.list_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async { devcontainer::list_running() }).await;
            let _ = this.update(cx, |this, cx| {
                this.dev.listing = false;
                match result {
                    Ok(containers) => this.dev.containers = containers,
                    Err(err) => {
                        this.dev.containers.clear();
                        this.dev.list_error = Some(format!("{err:#}"));
                    }
                }
                this.clamp_focus_index();
                cx.notify();
            });
        })
        .detach();
    }

    /// Resolve where the node's launches would run in the chosen container,
    /// off the UI thread: the repository there when it lives in the
    /// container, else the host directory mapped through its mounts.
    pub(super) fn check_dev_container(&mut self, cx: &mut Context<Self>) {
        self.dev.generation += 1;
        let generation = self.dev.generation;
        let Some(dev) = self.own_dev_container() else {
            self.dev.check = None;
            self.dev.checking = false;
            return;
        };
        let Some(container) = dev.container().map(str::to_string) else {
            self.dev.check = None;
            self.dev.checking = false;
            return;
        };
        let check: Box<dyn FnOnce() -> Result<String, String> + Send> = if dev.repo_on_host {
            let Some(host_dir) = self
                .own_files()
                .and_then(|files| files.ready_directory())
                .and_then(|dir| dir.host_path().map(std::path::Path::to_path_buf))
            else {
                // The resolved directory row already says what is missing.
                self.dev.check = None;
                self.dev.checking = false;
                return;
            };
            Box::new(move || {
                devcontainer::resolve_directory(&container, &host_dir, None)
                    .map(|(info, dir)| match info.remote_user {
                        Some(user) => format!("Runs in {dir} in {} as {user}", info.name),
                        None => format!("Runs in {dir} in {}", info.name),
                    })
                    .map_err(|err| format!("{err:#}"))
            })
        } else {
            let repo = self
                .own_files()
                .and_then(|files| files.repo_dir())
                .filter(|dir| dir.container_name().is_some())
                .map(|dir| dir.path_text());
            Box::new(move || check_repo_in_container(&container, repo.as_deref()))
        };
        self.dev.checking = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { check() }).await;
            let _ = this.update(cx, |this, cx| {
                if this.dev.generation != generation {
                    return;
                }
                this.dev.checking = false;
                this.dev.check = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_dev_container(
        &self,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let dev = self.own_dev_container();
        let active = cx.theme().list_active;
        let active_border = cx.theme().list_active_border;
        let foreground = cx.theme().foreground;
        let danger = cx.theme().danger;
        let runs_in_focused = self.field_nav_focused(TaskEditField::RunsIn);

        let runs_in = self.apply_focus_scroll_anchor(
            TaskEditField::RunsIn,
            h_flex()
                .id(super::field_anchor_id(TaskEditField::RunsIn))
                .items_center()
                .gap_2()
                .px_1()
                .rounded_md()
                .cursor_pointer()
                .when(runs_in_focused, |el| {
                    el.bg(active).border_1().border_color(active_border)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.enter_field_edit(TaskEditField::RunsIn, window, cx);
                }))
                .child(Self::render_field_label("Runs in", cx))
                .child(div().text_sm().text_color(foreground).child(if dev.is_some() {
                    "Dev container"
                } else {
                    "This machine"
                }))
                .child(div().text_xs().text_color(muted).child("Enter or click to switch")),
        );
        let Some(dev) = dev else {
            return v_flex().gap_2().child(runs_in);
        };

        let location_focused = self.field_nav_focused(TaskEditField::RepoLocation);
        let location = self.apply_focus_scroll_anchor(
            TaskEditField::RepoLocation,
            h_flex()
                .id(super::field_anchor_id(TaskEditField::RepoLocation))
                .items_center()
                .gap_2()
                .px_1()
                .rounded_md()
                .cursor_pointer()
                .when(location_focused, |el| {
                    el.bg(active).border_1().border_color(active_border)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.enter_field_edit(TaskEditField::RepoLocation, window, cx);
                }))
                .child(Self::render_field_label("Repository", cx))
                .child(div().text_sm().text_color(foreground).child(if dev.repo_on_host {
                    "On this machine, mounted into the container"
                } else {
                    "Inside the container"
                }))
                .child(div().text_xs().text_color(muted).child("Enter or click to switch")),
        );

        let fields = h_flex()
            .gap_2()
            .items_end()
            .flex_wrap()
            .child(self.apply_focus_scroll_anchor(
                TaskEditField::ContainerName,
                v_flex()
                    .id(super::field_anchor_id(TaskEditField::ContainerName))
                    .gap_1()
                    .w(gpui::px(200.))
                    .flex_shrink_0()
                    .child(Self::render_field_label("Container", cx))
                    .child(self.render_nav_input(
                        TaskEditField::ContainerName,
                        self.dev.container_input.clone(),
                        None,
                        window,
                        cx,
                    )),
            ));

        let refresh_focused = self.field_nav_focused(TaskEditField::ContainerRefresh);
        let list_header = h_flex()
            .gap_2()
            .items_center()
            .child(Self::render_field_label("Running containers", cx))
            .child(self.apply_focus_scroll_anchor(
                TaskEditField::ContainerRefresh,
                div()
                    .id(super::field_anchor_id(TaskEditField::ContainerRefresh))
                    .rounded_md()
                    .when(refresh_focused, |el| {
                        el.bg(active).border_1().border_color(active_border)
                    })
                    .child(
                        Button::new("task-edit-container-refresh")
                            .label(if self.dev.listing { "Listing…" } else { "Refresh" })
                            .outline()
                            .compact()
                            .disabled(self.dev.listing)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.enter_field_edit(TaskEditField::ContainerRefresh, window, cx);
                            })),
                    ),
            ));

        let chosen = dev.container().map(str::to_string);
        let mut list = v_flex().gap_0p5();
        for (index, container) in self.dev.containers.iter().enumerate() {
            let field = TaskEditField::ContainerChoice(index);
            let focused = self.field_nav_focused(field);
            let is_chosen = chosen.as_deref().is_some_and(|chosen| {
                chosen == container.name
                    || (chosen.len() >= 12 && container.id.starts_with(chosen))
            });
            let mut detail = container.image.clone();
            if let Some(folder) = &container.local_folder {
                detail = format!("{detail} · {folder}");
            }
            list = list.child(self.apply_focus_scroll_anchor(
                field,
                h_flex()
                    .id(ElementId::NamedInteger(
                        "task-edit-container-choice".into(),
                        index as u64,
                    ))
                    .gap_2()
                    .px_1()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(focused, |el| el.bg(active).border_1().border_color(active_border))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.enter_field_edit(field, window, cx);
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(foreground)
                            .child(if is_chosen { "●" } else { "○" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(foreground)
                            .flex_shrink_0()
                            .child(selectable_text(
                                gpui::SharedString::from(format!(
                                    "task-edit-container-name-{index}"
                                )),
                                container.name.clone(),
                                window,
                                cx,
                            )),
                    )
                    .when(container.is_dev_container(), |el| {
                        el.child(div().text_xs().text_color(muted).child("dev container"))
                    })
                    .child(
                        div().min_w_0().text_xs().text_color(muted).child(selectable_text(
                            gpui::SharedString::from(format!(
                                "task-edit-container-detail-{index}"
                            )),
                            detail,
                            window,
                            cx,
                        )),
                    ),
            ));
        }
        if self.dev.containers.is_empty() && !self.dev.listing && self.dev.list_error.is_none() {
            list = list.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("No running containers. Start the dev container, then Refresh."),
            );
        }

        let status = if self.dev.checking {
            Some((muted, "Checking the container…".to_string()))
        } else {
            match &self.dev.check {
                Some(Ok(text)) => Some((muted, text.clone())),
                Some(Err(err)) => Some((danger, err.clone())),
                None if chosen.is_none() => Some((muted, "Choose a container".to_string())),
                None => None,
            }
        };

        v_flex()
            .gap_2()
            .child(runs_in)
            .child(location)
            .child(fields)
            .when_some(status, |el, (color, text)| {
                el.child(div().text_xs().text_color(color).child(selectable_text(
                    "task-edit-dev-container-status",
                    text,
                    window,
                    cx,
                )))
            })
            .child(list_header)
            .when_some(self.dev.list_error.clone(), |el, err| {
                el.child(div().text_xs().text_color(danger).child(selectable_text(
                    "task-edit-container-list-error",
                    err,
                    window,
                    cx,
                )))
            })
            .child(list)
    }
}

/// The chosen container is running and `repo` (a path in it) is a git
/// repository there. Talks to Docker.
fn check_repo_in_container(container: &str, repo: Option<&str>) -> Result<String, String> {
    let exec = devcontainer::ContainerExec::connect(container).map_err(|err| format!("{err:#}"))?;
    let as_user = exec
        .user
        .as_deref()
        .map(|user| format!(" as {user}"))
        .unwrap_or_default();
    let Some(repo) = repo else {
        return Ok(format!(
            "{} is running{as_user}. Set the workspace directory to the repository's path in it.",
            exec.name
        ));
    };
    let [config, safe] = tod_store::fleet::workdir::CONTAINER_GIT_CONFIG;
    let out = exec
        .output("/", "git", &[config, safe, "-C", repo, "rev-parse", "--show-toplevel"])
        .map_err(|err| format!("{err:#}"))?;
    if !out.status.success() {
        return Err(format!(
            "{repo} is not a git repository in {}: {}",
            exec.name,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(format!("Repository {repo} in {}{as_user}", exec.name))
}
