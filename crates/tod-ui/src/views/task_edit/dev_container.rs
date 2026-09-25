//! The Files capability's "Runs in" section: this machine, a running dev
//! container the user picks from `docker ps` (or names by hand), or a cloud
//! sandbox from the workspace's, which can also create one: from an image
//! (the default from Settings unless one is given) or as a fork of another
//! sandbox.
//!
//! The repository usually lives in the container, and the workspace
//! directory is its path there. "Repository" switches to one on this machine
//! mounted into the container; the directory in the container then follows
//! from its mounts. In a sandbox the repository is always there.
//!
//! Docker and the sandbox are only ever called off the UI thread. The container name
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
use tod_store::fleet::sandbox::{self as sandboxes, ListedSandbox, NewSandboxSource};
use tod_store::fleet::{DevContainerSetting, FleetMutation};

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

pub(super) struct DevContainerPanel {
    pub(super) container_input: Entity<InputState>,
    /// Running containers from the last `docker ps`, dev containers first,
    /// or the workspace's sandboxes.
    containers: Vec<ContainerSummary>,
    /// The sandboxes behind `containers`, when those are sandboxes.
    sandboxes: Vec<ListedSandbox>,
    listing: bool,
    list_error: Option<String>,
    /// Bumped per listing, so one of the other kind that ends late is dropped.
    list_generation: u64,
    /// A new sandbox: its image (empty means the default from Settings).
    pub(super) new_image_input: Entity<InputState>,
    /// A new sandbox: its name.
    pub(super) new_name_input: Entity<InputState>,
    /// A new sandbox forks `fork_source` instead of starting from an image.
    new_from_fork: bool,
    fork_source: Option<String>,
    /// What creating a sandbox is doing, while it runs.
    creating: Option<String>,
    create_error: Option<String>,
    /// The default image from Settings, shown under the image field.
    default_image: String,
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
            InputState::new(window, cx).placeholder("Enter to edit · Name")
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
        let new_image_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Enter to edit · Empty uses the default")
        });
        let new_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Enter to edit · Sandbox name"));
        Self {
            container_input,
            containers: Vec::new(),
            sandboxes: Vec::new(),
            listing: false,
            list_error: None,
            list_generation: 0,
            new_image_input,
            new_name_input,
            new_from_fork: false,
            fork_source: None,
            creating: None,
            create_error: None,
            default_image: String::new(),
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

    /// Whether a new sandbox forks one (else it starts from an image).
    pub(super) fn new_from_fork(&self) -> bool {
        self.new_from_fork
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
        self.reset_new_sandbox(window, cx);
        if self.own_dev_container().is_some() {
            if self.dev.containers.is_empty() && !self.dev.listing {
                self.refresh_containers(cx);
            }
            self.check_dev_container(cx);
        }
    }

    /// A new sandbox's draft for this node: named after it, from the
    /// default image. A creation still running keeps its own.
    fn reset_new_sandbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dev.creating.is_some() {
            return;
        }
        self.dev.create_error = None;
        let root = self.fleet.paths().root().to_path_buf();
        self.dev.default_image = sandboxes::account_settings(&root).1;
        let name = sandboxes::suggested_name(&self.loaded_slug);
        self.dev.new_name_input.update(cx, |input, cx| input.set_value(name, window, cx));
        self.dev.new_image_input.update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// "Start from": an image ↔ a fork of another sandbox.
    pub(super) fn toggle_new_sandbox_source(&mut self, cx: &mut Context<Self>) {
        self.dev.new_from_fork = !self.dev.new_from_fork;
        self.dev.create_error = None;
        self.clamp_focus_index();
        cx.notify();
    }

    /// The next listed sandbox to fork.
    pub(super) fn cycle_fork_source(&mut self, cx: &mut Context<Self>) {
        let names: Vec<&str> = self.dev.sandboxes.iter().map(|s| s.name.as_str()).collect();
        if names.is_empty() {
            self.dev.create_error = Some("No sandboxes to fork. Refresh lists them.".into());
            cx.notify();
            return;
        }
        let next = match self.dev.fork_source.as_deref().and_then(|c| names.iter().position(|n| *n == c)) {
            Some(i) => names[(i + 1) % names.len()],
            None => names[0],
        };
        self.dev.fork_source = Some(next.to_string());
        self.dev.create_error = None;
        cx.notify();
    }

    /// Create the drafted sandbox off the UI thread, then use it for this
    /// node.
    pub(super) fn create_sandbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dev.creating.is_some() {
            return;
        }
        let Some(node_id) = self.task_id() else {
            return;
        };
        let name = input_text(&self.dev.new_name_input, cx).trim().to_string();
        if let Err(err) = sandboxes::validate_name(&name) {
            self.dev.create_error = Some(format!("{err:#}"));
            cx.notify();
            return;
        }
        let source = if self.dev.new_from_fork {
            match self.dev.fork_source.clone() {
                Some(source) => NewSandboxSource::Fork(source),
                None => {
                    self.dev.create_error = Some("Choose a sandbox to fork.".into());
                    cx.notify();
                    return;
                }
            }
        } else {
            NewSandboxSource::Image(input_text(&self.dev.new_image_input, cx).trim().to_string())
        };
        self.dev.creating = Some(match &source {
            NewSandboxSource::Fork(from) => format!("Forking {from} into {name}…"),
            NewSandboxSource::Image(_) => format!("Creating {name}…"),
        });
        self.dev.create_error = None;
        cx.notify();
        let root = self.fleet.paths().root().to_path_buf();
        let (tx, rx) = async_channel::unbounded::<String>();
        let created = name.clone();
        let task = cx.background_spawn(async move {
            let mut sandboxes = sandboxes::Sandboxes::load(&root)?;
            // Agents run in every sandbox the app makes.
            sandboxes.create(&created, &source, true, false, &mut |step| {
                let _ = tx.try_send(step.to_string());
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(step) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.dev.creating = Some(step);
                    cx.notify();
                });
            }
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.dev.creating = None;
                match result {
                    Ok(_) => {
                        // Still on the node it was made for: use it there.
                        if this.task_id().as_deref() == Some(node_id.as_str())
                            && this.runs_in_sandbox()
                        {
                            this.dev.container_input.update(cx, |input, cx| {
                                input.set_value(name.clone(), window, cx);
                            });
                            this.save_dev_container(
                                Some(DevContainerSetting {
                                    container: Some(name),
                                    repo_on_host: false,
                                    sandbox: true,
                                }),
                                cx,
                            );
                        }
                    }
                    Err(err) => this.dev.create_error = Some(format!("{err:#}")),
                }
                // A failed setup can leave the sandbox made: list it either way.
                this.refresh_containers(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// "Runs in": this machine → dev container → cloud sandbox → this machine.
    pub(super) fn toggle_dev_container(&mut self, cx: &mut Context<Self>) {
        if self.own_files().is_none() {
            return;
        }
        let next = match self.own_dev_container() {
            Some(dev) if dev.sandbox => None,
            Some(_) => Some(DevContainerSetting {
                sandbox: true,
                ..Default::default()
            }),
            None => Some(DevContainerSetting {
                container: Some(input_text(&self.dev.container_input, cx))
                    .filter(|c| !c.trim().is_empty()),
                ..Default::default()
            }),
        };
        let turning_on = next.is_some();
        // What was listed is the other kind's.
        self.dev.containers.clear();
        self.dev.sandboxes.clear();
        if self.save_dev_container(next, cx) && turning_on {
            self.refresh_containers(cx);
        }
    }

    /// Whether this node's own work runs in a cloud sandbox.
    pub(super) fn runs_in_sandbox(&self) -> bool {
        self.own_dev_container().is_some_and(|dev| dev.sandbox)
    }

    /// Where the node's own repository is, when not on this machine.
    pub(super) fn remote_place(&self) -> &'static str {
        if self.runs_in_sandbox() {
            "the sandbox"
        } else {
            "the dev container"
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

    /// Whether this node's own repository lives in its dev container or
    /// sandbox.
    pub(super) fn repo_in_container(&self) -> bool {
        self.own_dev_container()
            .is_some_and(|dev| dev.repo_is_remote())
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
        let sandbox = self.runs_in_sandbox();
        let valid = if sandbox {
            sandboxes::validate_name(&container)
        } else {
            devcontainer::validate_container_ref(&container)
        };
        if !container.is_empty()
            && let Err(err) = valid
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
                sandbox,
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
                repo_on_host: dev.repo_on_host && !dev.sandbox,
                sandbox: dev.sandbox,
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

    /// `docker ps`, or the workspace's sandboxes, off the UI thread. A
    /// listing still running is superseded: switching from a dev container
    /// to a sandbox must not wait on (or show) a slow `docker ps`.
    pub(super) fn refresh_containers(&mut self, cx: &mut Context<Self>) {
        self.dev.list_generation += 1;
        let generation = self.dev.list_generation;
        self.dev.listing = true;
        self.dev.list_error = None;
        cx.notify();
        let sandbox = self.runs_in_sandbox();
        let root = self.fleet.paths().root().to_path_buf();
        cx.spawn(async move |this, cx| {
            // Settings may have changed the default image since the node loaded.
            let (result, default_image) = cx
                .background_spawn(async move {
                    if sandbox {
                        let default_image = sandboxes::account_settings(&root).1;
                        (sandboxes::list(&root).map(Listing::Sandboxes), Some(default_image))
                    } else {
                        (devcontainer::list_running().map(Listing::Containers), None)
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.dev.list_generation != generation {
                    return;
                }
                this.dev.listing = false;
                if let Some(default_image) = default_image {
                    this.dev.default_image = default_image;
                }
                match result {
                    Ok(Listing::Containers(containers)) => {
                        this.dev.sandboxes.clear();
                        this.dev.containers = containers;
                    }
                    Ok(Listing::Sandboxes(listed)) => {
                        this.dev.containers = listed.iter().map(sandbox_choice).collect();
                        let still_there = this.dev.fork_source.as_deref().is_some_and(|source| {
                            listed.iter().any(|s| s.name == source)
                        });
                        if !still_there {
                            this.dev.fork_source = listed.first().map(|s| s.name.clone());
                        }
                        this.dev.sandboxes = listed;
                    }
                    Err(err) => {
                        this.dev.containers.clear();
                        this.dev.sandboxes.clear();
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
        let check: Box<dyn FnOnce() -> Result<String, String> + Send> = if dev.sandbox {
            let repo = self
                .own_files()
                .and_then(|files| files.repo_dir())
                .filter(|dir| dir.sandbox_name().is_some())
                .map(|dir| dir.path_text());
            Box::new(move || check_repo_in_sandbox(&container, repo.as_deref()))
        } else if dev.repo_on_host {
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
                .child(div().text_sm().text_color(foreground).child(match &dev {
                    Some(dev) if dev.sandbox => "Cloud sandbox",
                    Some(_) => "Dev container",
                    None => "This machine",
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
                    .child(Self::render_field_label(
                        if dev.sandbox { "Sandbox" } else { "Container" },
                        cx,
                    ))
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
            .child(Self::render_field_label(
                if dev.sandbox {
                    "Sandboxes"
                } else {
                    "Running containers"
                },
                cx,
            ))
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
            list = list.child(div().text_xs().text_color(muted).child(if dev.sandbox {
                "No sandboxes in the workspace yet. Create one below."
            } else {
                "No running containers. Start the dev container, then Refresh."
            }));
        }

        let status = if self.dev.checking {
            let checking = if dev.sandbox {
                "Checking the sandbox (this wakes it)…"
            } else {
                "Checking the container…"
            };
            Some((muted, checking.to_string()))
        } else {
            match &self.dev.check {
                Some(Ok(text)) => Some((muted, text.clone())),
                Some(Err(err)) => Some((danger, err.clone())),
                None if chosen.is_none() => {
                    let choose = if dev.sandbox { "Choose a sandbox" } else { "Choose a container" };
                    Some((muted, choose.to_string()))
                }
                None => None,
            }
        };

        v_flex()
            .gap_2()
            .child(runs_in)
            // A sandbox always holds its repository.
            .when(!dev.sandbox, |el| el.child(location))
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
            .when(dev.sandbox, |el| el.child(self.render_new_sandbox(muted, window, cx)))
    }

    /// "New sandbox": start from an image or fork one, a name, and Create.
    fn render_new_sandbox(
        &self,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = cx.theme().list_active;
        let active_border = cx.theme().list_active_border;
        let foreground = cx.theme().foreground;
        let danger = cx.theme().danger;
        let creating = self.dev.creating.is_some();
        let toggle_row = |field: TaskEditField, label: &'static str, value: String, hint: &'static str, cx: &mut Context<Self>| {
            let focused = self.field_nav_focused(field);
            self.apply_focus_scroll_anchor(
                field,
                h_flex()
                    .id(super::field_anchor_id(field))
                    .items_center()
                    .gap_2()
                    .px_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(focused, |el| el.bg(active).border_1().border_color(active_border))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.enter_field_edit(field, window, cx);
                    }))
                    .child(Self::render_field_label(label, cx))
                    .child(div().text_sm().text_color(foreground).child(value))
                    .child(div().text_xs().text_color(muted).child(hint)),
            )
        };
        let source = toggle_row(
            TaskEditField::NewSandboxSource,
            "Start from",
            if self.dev.new_from_fork { "A fork of a sandbox" } else { "An image" }.to_string(),
            "Enter or click to switch",
            cx,
        );
        let from = if self.dev.new_from_fork {
            let value = match self.dev.fork_source.as_deref() {
                Some(name) => match self.dev.sandboxes.iter().find(|s| s.name == name) {
                    Some(listed) => format!("{name} ({})", listed.status.to_lowercase()),
                    None => name.to_string(),
                },
                None => "No sandbox to fork".to_string(),
            };
            toggle_row(TaskEditField::NewSandboxForkSource, "Fork", value, "Enter or click for the next", cx)
                .into_any_element()
        } else {
            v_flex()
                .gap_1()
                .child(self.apply_focus_scroll_anchor(
                    TaskEditField::NewSandboxImage,
                    v_flex()
                        .id(super::field_anchor_id(TaskEditField::NewSandboxImage))
                        .gap_1()
                        .child(Self::render_field_label("Image", cx))
                        .child(self.render_nav_input(
                            TaskEditField::NewSandboxImage,
                            self.dev.new_image_input.clone(),
                            None,
                            window,
                            cx,
                        )),
                ))
                .child(div().text_xs().text_color(muted).child(selectable_text(
                    "task-edit-new-sandbox-default-image",
                    format!("Empty uses {} (Settings → Cloud sandboxes)", self.dev.default_image),
                    window,
                    cx,
                )))
                .into_any_element()
        };
        let name = self.apply_focus_scroll_anchor(
            TaskEditField::NewSandboxName,
            v_flex()
                .id(super::field_anchor_id(TaskEditField::NewSandboxName))
                .gap_1()
                .w(gpui::px(260.))
                .child(Self::render_field_label("Name", cx))
                .child(self.render_nav_input(
                    TaskEditField::NewSandboxName,
                    self.dev.new_name_input.clone(),
                    None,
                    window,
                    cx,
                )),
        );
        let create_focused = self.field_nav_focused(TaskEditField::NewSandboxCreate);
        // In a row, so the button is only as wide as its label.
        let create = h_flex().child(self.apply_focus_scroll_anchor(
            TaskEditField::NewSandboxCreate,
            div()
                .id(super::field_anchor_id(TaskEditField::NewSandboxCreate))
                .rounded_md()
                .when(create_focused, |el| el.bg(active).border_1().border_color(active_border))
                .child(
                    Button::new("task-edit-new-sandbox-create")
                        .label(if creating {
                            "Creating…"
                        } else if self.dev.new_from_fork {
                            "Fork and use it"
                        } else {
                            "Create and use it"
                        })
                        .outline()
                        .compact()
                        .disabled(creating)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.enter_field_edit(TaskEditField::NewSandboxCreate, window, cx);
                        })),
                ),
        ));
        let status = match (&self.dev.creating, &self.dev.create_error) {
            (Some(step), _) => Some((muted, step.clone())),
            (None, Some(err)) => Some((danger, err.clone())),
            (None, None) => None,
        };
        v_flex()
            .gap_2()
            .pt_2()
            .child(Self::render_field_label("New sandbox", cx))
            .child(source)
            .child(from)
            .child(name)
            .child(create)
            .when_some(status, |el, (color, text)| {
                el.child(div().text_xs().text_color(color).child(selectable_text(
                    "task-edit-new-sandbox-status",
                    text,
                    window,
                    cx,
                )))
            })
    }
}

/// What a listing found.
enum Listing {
    Containers(Vec<ContainerSummary>),
    Sandboxes(Vec<ListedSandbox>),
}

/// A sandbox, shown as one of the list's choices: its state, image, and
/// who made it.
fn sandbox_choice(listed: &ListedSandbox) -> ContainerSummary {
    let mut detail = format!("{} · {}", listed.status.to_lowercase(), listed.image);
    if let Some(owner) = &listed.owner {
        detail = format!("{detail} · {owner}");
    }
    ContainerSummary {
        id: listed.name.clone(),
        name: listed.name.clone(),
        image: detail,
        status: listed.status.clone(),
        local_folder: None,
    }
}

/// The sandbox can be reached (made ready for tod if it is not) and `repo`
/// (a path in it) is a git repository there. Talks to Blaxel.
fn check_repo_in_sandbox(sandbox: &str, repo: Option<&str>) -> Result<String, String> {
    let exec = sandboxes::SandboxExec::new(sandbox);
    let Some(repo) = repo else {
        exec.output("/", "true", &[]).map_err(|err| format!("{err:#}"))?;
        return Ok(format!(
            "Sandbox {sandbox} is ready. Set the workspace directory to the repository's path in it."
        ));
    };
    let out = exec
        .output("/", "git", &["-C", repo, "rev-parse", "--show-toplevel"])
        .map_err(|err| format!("{err:#}"))?;
    if !out.status.success() {
        return Err(format!(
            "{repo} is not a git repository in sandbox {sandbox}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(format!("Repository {repo} in sandbox {sandbox}"))
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
