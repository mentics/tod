//! The Files capability's "Runs in" section: this machine, a running dev
//! container the user picks from `docker ps` (or names by hand), or cloud
//! sandboxes. With sandboxes, this is only what each node's own sandbox
//! starts from — an image (the default from Settings unless one is given)
//! or a fork of one of the workspace's sandboxes; each is made the first
//! time its node needs its files (`tod_store::fleet::provision`).
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
use tod_store::fleet::sandbox::{self as sandboxes, ListedSandbox};
use tod_store::fleet::{DevContainerSetting, FleetMutation, SandboxFrom};

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
    /// The image each node's sandbox starts from (empty means the default
    /// from Settings).
    pub(super) new_image_input: Entity<InputState>,
    /// Why the chosen fork source cannot be used.
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
    _subscriptions: [Subscription; 2],
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
        let new_image_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Enter to edit · Empty uses the default")
        });
        let _subscriptions = [subscribe(&container_input, cx), subscribe(&new_image_input, cx)];
        Self {
            container_input,
            containers: Vec::new(),
            sandboxes: Vec::new(),
            listing: false,
            list_error: None,
            list_generation: 0,
            new_image_input,
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
        self.dev.create_error = None;
        let root = self.fleet.paths().root().to_path_buf();
        self.dev.default_image = sandboxes::account_settings(&root).1;
        let image = match &dev.sandbox_from {
            SandboxFrom::Image(image) => image.clone(),
            SandboxFrom::Fork(_) => String::new(),
        };
        self.dev.new_image_input.update(cx, |input, cx| input.set_value(image, window, cx));
        if self.own_dev_container().is_some() {
            if self.dev.containers.is_empty() && !self.dev.listing {
                self.refresh_containers(cx);
            }
            self.check_dev_container(cx);
        }
    }

    /// What each node's sandbox starts from, when it runs in sandboxes.
    fn sandbox_from(&self) -> Option<SandboxFrom> {
        self.own_dev_container()
            .filter(|dev| dev.sandbox)
            .map(|dev| dev.sandbox_from)
    }

    /// Whether each node's sandbox is a fork (else it starts from an image).
    pub(super) fn sandbox_is_fork(&self) -> bool {
        matches!(self.sandbox_from(), Some(SandboxFrom::Fork(_)))
    }

    /// Save what each node's sandbox starts from.
    fn save_sandbox_from(&mut self, from: SandboxFrom, cx: &mut Context<Self>) {
        let Some(dev) = self.own_dev_container().filter(|dev| dev.sandbox) else {
            return;
        };
        self.dev.create_error = None;
        self.save_dev_container(
            Some(DevContainerSetting {
                sandbox_from: from,
                ..dev
            }),
            cx,
        );
    }

    /// "Start from": an image ↔ a fork of one of the workspace's sandboxes.
    pub(super) fn toggle_new_sandbox_source(&mut self, cx: &mut Context<Self>) {
        let next = match self.sandbox_from() {
            Some(SandboxFrom::Fork(_)) => {
                SandboxFrom::Image(input_text(&self.dev.new_image_input, cx).trim().to_string())
            }
            Some(SandboxFrom::Image(_)) => SandboxFrom::Fork(
                self.dev
                    .sandboxes
                    .first()
                    .map(|s| s.name.clone())
                    .unwrap_or_default(),
            ),
            None => return,
        };
        self.save_sandbox_from(next, cx);
        self.clamp_focus_index();
        cx.notify();
    }

    /// Fork the next listed sandbox.
    pub(super) fn cycle_fork_source(&mut self, cx: &mut Context<Self>) {
        let names: Vec<&str> = self.dev.sandboxes.iter().map(|s| s.name.as_str()).collect();
        if names.is_empty() {
            self.dev.create_error = Some("No sandboxes to fork. Refresh lists them.".into());
            cx.notify();
            return;
        }
        let current = match self.sandbox_from() {
            Some(SandboxFrom::Fork(name)) => Some(name),
            _ => None,
        };
        let next = match current.as_deref().and_then(|c| names.iter().position(|n| *n == c)) {
            Some(i) => names[(i + 1) % names.len()],
            None => names[0],
        };
        let next = next.to_string();
        self.save_sandbox_from(SandboxFrom::Fork(next), cx);
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

    /// "Runs in" puts the repository in a container or sandbox, whether or
    /// not one is chosen yet: its path is not a path on this machine.
    pub(super) fn repo_path_is_remote(&self) -> bool {
        self.own_dev_container()
            .is_some_and(|dev| dev.sandbox || !dev.repo_on_host)
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
        // Sandboxes: the image, when they start from one.
        if let Some(from) = self.sandbox_from() {
            if matches!(from, SandboxFrom::Image(_)) {
                let image = input_text(&self.dev.new_image_input, cx).trim().to_string();
                self.save_sandbox_from(SandboxFrom::Image(image), cx);
            }
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
        let current = self.own_dev_container().unwrap_or_default();
        self.save_dev_container(
            Some(DevContainerSetting {
                container: (!container.is_empty()).then_some(container),
                sandbox,
                ..current
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
        // A listed sandbox is one to fork.
        if self.sandbox_from().is_some() {
            self.save_sandbox_from(SandboxFrom::Fork(name), cx);
            return;
        }
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
    /// the stored setting is now `setting`: not yet when worktrees or
    /// sandboxes made from the old one must be removed first, which the
    /// user confirms.
    fn save_dev_container(
        &mut self,
        setting: Option<DevContainerSetting>,
        cx: &mut Context<Self>,
    ) -> bool {
        let normalize = |dev: Option<DevContainerSetting>| {
            dev.map(|dev| DevContainerSetting {
                container: if dev.sandbox {
                    None
                } else {
                    dev.container().map(str::to_string)
                },
                repo_on_host: dev.repo_on_host && !dev.sandbox,
                sandbox: dev.sandbox,
                sandbox_from: if dev.sandbox {
                    dev.sandbox_from
                } else {
                    SandboxFrom::default()
                },
            })
        };
        let setting = normalize(setting);
        if normalize(self.own_dev_container()) == setting {
            return true;
        }
        let (repo, use_worktree, _) = self.files_settings();
        let affected = self.locations_changed_by(repo.as_deref(), use_worktree, setting.as_ref());
        if !affected.is_empty() {
            let intro = match &setting {
                Some(dev) if dev.sandbox => format!(
                    "Each node's sandbox will start from {}. These were made from the old \
                     settings, and are removed first (each branch is pushed before it goes).",
                    dev.sandbox_from.describe()
                ),
                _ => "These were made from where the files were, and are removed first (each \
                      branch is pushed before it goes)."
                    .to_string(),
            };
            let listing = setting.is_some();
            self.guard_files_change(
                affected,
                "Change where the files are?".into(),
                intro,
                "Remove and change",
                move |this, window, cx| {
                    if this.write_dev_container(setting, cx) && listing {
                        this.refresh_containers(cx);
                    }
                    this.load_dev_container(window, cx);
                },
                cx,
            );
            return false;
        }
        self.write_dev_container(setting, cx)
    }

    fn write_dev_container(
        &mut self,
        setting: Option<DevContainerSetting>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(node_id) = self.task_id() else {
            return false;
        };
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
        let Some(dev) = self.own_dev_container() else {
            self.dev.check = None;
            self.dev.checking = false;
            return;
        };
        let use_worktree = self.own_files().is_some_and(|files| files.use_worktree);
        if dev.sandbox {
            // This node's own sandbox once made, else the one each is forked
            // from. An image is only checked by making a sandbox from it.
            let made = self.own_files().and_then(|files| files.repo_sandbox()).map(str::to_string);
            let target = made.clone().or(match &dev.sandbox_from {
                SandboxFrom::Fork(name) if !name.trim().is_empty() => Some(name.trim().to_string()),
                _ => None,
            });
            let repo = self.own_files().and_then(|files| files.repo()).map(str::to_string);
            let Some(target) = target else {
                self.dev.checking = false;
                self.dev.check = Some(Ok(format!(
                    "Each node that needs its files gets a sandbox of its own from {}, made \
                     then. The image must hold the repository{}.",
                    dev.sandbox_from.describe(),
                    repo.map(|repo| format!(" at {repo}")).unwrap_or_default()
                )));
                return;
            };
            let prefix = match made {
                Some(_) => "This node's sandbox: ",
                None => "The sandbox each node forks: ",
            };
            self.run_dev_container_check(
                Box::new(move || {
                    check_repo_in_sandbox(&target, repo.as_deref(), true)
                        .map(|found| format!("{prefix}{found}"))
                        .map_err(|err| format!("{prefix}{err}"))
                }),
                cx,
            );
            return;
        }
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
            Box::new(move || check_repo_in_container(&container, repo.as_deref(), use_worktree))
        };
        self.run_dev_container_check(check, cx);
    }

    fn run_dev_container_check(
        &mut self,
        check: Box<dyn FnOnce() -> Result<String, String> + Send>,
        cx: &mut Context<Self>,
    ) {
        let generation = self.dev.generation;
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
                    "Sandboxes to fork"
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
        let sandbox_fork = match &dev.sandbox_from {
            SandboxFrom::Fork(name) if dev.sandbox => Some(name.clone()),
            _ => None,
        };
        let mut list = v_flex().gap_0p5();
        for (index, container) in self.dev.containers.iter().enumerate() {
            let field = TaskEditField::ContainerChoice(index);
            let focused = self.field_nav_focused(field);
            let is_chosen = chosen.as_deref().is_some_and(|chosen| {
                chosen == container.name
                    || (chosen.len() >= 12 && container.id.starts_with(chosen))
            });
            let is_chosen = is_chosen
                || matches!(&sandbox_fork, Some(fork) if *fork == container.name);
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
                "No sandboxes in the workspace to fork."
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
                None if chosen.is_none() && !dev.sandbox => {
                    Some((muted, "Choose a container".to_string()))
                }
                None => None,
            }
        };

        let sandbox = dev.sandbox;
        let forking = matches!(dev.sandbox_from, SandboxFrom::Fork(_));
        v_flex()
            .gap_2()
            .child(runs_in)
            // A sandbox always holds its repository, and each node's is
            // made for it: there is none to name.
            .when(!sandbox, |el| el.child(location).child(fields))
            .when(sandbox, |el| el.child(self.render_sandbox_from(muted, window, cx)))
            .when_some(status, |el, (color, text)| {
                el.child(div().text_xs().text_color(color).child(selectable_text(
                    "task-edit-dev-container-status",
                    text,
                    window,
                    cx,
                )))
            })
            // Sandboxes are listed only to pick the one to fork.
            .when(!sandbox || forking, |el| {
                el.child(list_header)
                    .when_some(self.dev.list_error.clone(), |el, err| {
                        el.child(div().text_xs().text_color(danger).child(selectable_text(
                            "task-edit-container-list-error",
                            err,
                            window,
                            cx,
                        )))
                    })
                    .child(list)
            })
    }

    /// What each node's sandbox starts from: an image, or a fork of one of
    /// the listed sandboxes.
    fn render_sandbox_from(
        &self,
        muted: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = cx.theme().list_active;
        let active_border = cx.theme().list_active_border;
        let foreground = cx.theme().foreground;
        let danger = cx.theme().danger;
        let from = self.sandbox_from().unwrap_or_default();
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
            "Each node's sandbox starts from",
            match &from {
                SandboxFrom::Fork(_) => "A fork of a sandbox",
                SandboxFrom::Image(_) => "An image",
            }
            .to_string(),
            "Enter or click to switch",
            cx,
        );
        let detail = match &from {
            SandboxFrom::Fork(name) => {
                let value = if name.trim().is_empty() {
                    "None chosen: pick one from the list".to_string()
                } else {
                    match self.dev.sandboxes.iter().find(|s| s.name == *name) {
                        Some(listed) => format!("{name} ({})", listed.status.to_lowercase()),
                        None => name.clone(),
                    }
                };
                toggle_row(TaskEditField::NewSandboxForkSource, "Fork", value, "Enter or click for the next", cx)
                    .into_any_element()
            }
            SandboxFrom::Image(_) => v_flex()
                .gap_1()
                .child(self.apply_focus_scroll_anchor(
                    TaskEditField::NewSandboxImage,
                    v_flex()
                        .id(super::field_anchor_id(TaskEditField::NewSandboxImage))
                        .gap_1()
                        .w(gpui::px(320.))
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
                .into_any_element(),
        };
        v_flex()
            .gap_2()
            .child(source)
            .child(detail)
            .when_some(self.dev.create_error.clone(), |el, err| {
                el.child(div().text_xs().text_color(danger).child(selectable_text(
                    "task-edit-new-sandbox-status",
                    err,
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
/// (a path in it) is a directory there: a git repository when the node
/// uses a worktree. Talks to Blaxel.
fn check_repo_in_sandbox(
    sandbox: &str,
    repo: Option<&str>,
    use_worktree: bool,
) -> Result<String, String> {
    let exec = sandboxes::SandboxExec::new(sandbox);
    let Some(repo) = repo else {
        exec.output("/", "true", &[]).map_err(|err| format!("{err:#}"))?;
        return Ok(format!(
            "Sandbox {sandbox} is ready. Set the workspace directory to a path in it."
        ));
    };
    let place = format!("sandbox {sandbox}");
    let git = exec
        .output("/", "git", &["-C", repo, "rev-parse", "--show-toplevel"])
        .map_err(|err| format!("{err:#}"))?;
    let is_dir = || -> Result<bool, String> {
        exec.output("/", "test", &["-d", repo])
            .map(|out| out.status.success())
            .map_err(|err| format!("{err:#}"))
    };
    directory_verdict(repo, &place, "", git, is_dir, use_worktree)
}

/// What checking `repo` in `place` found: a git repository, or (without a
/// worktree, which needs one) any directory. `is_dir` runs only when `git`
/// failed.
fn directory_verdict(
    repo: &str,
    place: &str,
    as_user: &str,
    git: std::process::Output,
    is_dir: impl FnOnce() -> Result<bool, String>,
    use_worktree: bool,
) -> Result<String, String> {
    if git.status.success() {
        return Ok(format!("Repository {repo} in {place}{as_user}"));
    }
    if !is_dir()? {
        return Err(format!("{repo} does not exist in {place}"));
    }
    if use_worktree {
        return Err(format!(
            "{repo} in {place} is not a git repository, which a worktree needs: {}",
            String::from_utf8_lossy(&git.stderr).trim()
        ));
    }
    Ok(format!("Directory {repo} in {place}{as_user} (not a git repository)"))
}

/// The chosen container is running and `repo` (a path in it) is a
/// directory there: a git repository when the node uses a worktree. Talks
/// to Docker.
fn check_repo_in_container(
    container: &str,
    repo: Option<&str>,
    use_worktree: bool,
) -> Result<String, String> {
    let exec = devcontainer::ContainerExec::connect(container).map_err(|err| format!("{err:#}"))?;
    let as_user = exec
        .user
        .as_deref()
        .map(|user| format!(" as {user}"))
        .unwrap_or_default();
    let Some(repo) = repo else {
        return Ok(format!(
            "{} is running{as_user}. Set the workspace directory to a path in it.",
            exec.name
        ));
    };
    let [config, safe] = tod_store::fleet::workdir::CONTAINER_GIT_CONFIG;
    let git = exec
        .output("/", "git", &[config, safe, "-C", repo, "rev-parse", "--show-toplevel"])
        .map_err(|err| format!("{err:#}"))?;
    let is_dir = || -> Result<bool, String> {
        exec.output("/", "test", &["-d", repo])
            .map(|out| out.status.success())
            .map_err(|err| format!("{err:#}"))
    };
    directory_verdict(repo, &exec.name, &as_user, git, is_dir, use_worktree)
}
