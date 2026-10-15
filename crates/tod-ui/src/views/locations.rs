//! The Locations dialog (status bar → "Locations"): where nodes' files come
//! from, configured once for the whole app. A location is a workspace
//! directory plus where it runs (this machine, a running dev container, or
//! cloud sandboxes) and whether each node gets a worktree
//! (`tod_store::fleet::FilesPlace`). A node's Files capability only picks one.
//!
//! Editing or deleting a location a node uses is confirmed first with its
//! impact: the nodes that picked it, and the worktrees and sandboxes made
//! from it that the change would leave behind. Those are removed (each branch
//! pushed first) in the shared `files_impact` dialog before the change is
//! stored. A rename, or any edit that leaves the settings a worktree was made
//! from alone, removes nothing.
//!
//! Docker, Blaxel, git and the store are only called off the UI thread.

use crate::interview::TodPaths;
use crate::ui::files_impact::confirm_files_change;
use crate::ui::off_thread::off_thread;
use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, ElementId, Entity, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Disableable, WindowExt, h_flex, v_flex};
use std::sync::Arc;
use tod_agent::devcontainer::{self, ContainerSummary};
use tod_store::fleet::places::{self, PlaceImpact};
use tod_store::fleet::sandbox::{self as sandboxes, ListedSandbox};
use tod_store::fleet::{DevContainerSetting, FilesPlace, FleetStore, SandboxFrom};

/// Open the dialog.
pub fn open_locations_dialog(
    window: &mut Window,
    cx: &mut App,
    fleet: Arc<FleetStore>,
    paths: TodPaths,
) {
    let view = cx.new(|cx| LocationsDialog::new(fleet, paths, window, cx));
    window.open_dialog(cx, move |d, _, _| {
        d.title("Locations")
            .w(px(720.))
            .overlay(true)
            .overlay_closable(true)
            .keyboard(true)
            .close_button(true)
            .child(view.clone())
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunsIn {
    Machine,
    DevContainer,
    Sandbox,
}

enum Mode {
    List,
    /// Creating (`editing` is `None`) or editing a location.
    Edit,
    /// A change to a location nodes use, waiting for the user's yes.
    Confirm(Pending),
}

enum Change {
    Save(FilesPlace),
    Delete(FilesPlace),
}

struct Pending {
    change: Change,
    impact: PlaceImpact,
}

struct LocationsDialog {
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    mode: Mode,
    places: Vec<FilesPlace>,
    /// How many nodes use each location, by id.
    usage: Vec<(String, usize)>,
    /// The location being edited; `None` while creating one.
    editing: Option<FilesPlace>,
    name: Entity<InputState>,
    repo: Entity<InputState>,
    container: Entity<InputState>,
    image: Entity<InputState>,
    use_worktree: bool,
    runs_in: RunsIn,
    repo_on_host: bool,
    fork: bool,
    fork_name: String,
    default_image: String,
    /// The form that builds an image holding repositories: whether it is
    /// open, its inputs, whether a build is running, and how the last one
    /// went (`Ok` is what to tell the user, `Err` what went wrong).
    build_open: bool,
    build_base: Entity<InputState>,
    build_name: Entity<InputState>,
    build_repos: Entity<TextareaState>,
    building: bool,
    build_result: Option<Result<String, String>>,
    listed: Vec<ContainerSummary>,
    listed_sandboxes: Vec<ListedSandbox>,
    listing: bool,
    list_error: Option<String>,
    /// Where launches would run, or why they cannot, per the last check.
    check: Option<Result<String, String>>,
    checking: bool,
    /// Bumped per check, so a late result for an edit since changed is dropped.
    generation: u64,
    busy: bool,
    error: Option<String>,
}

/// A `UserAction` for the Locations dialog, with the buttons its current
/// screen showed (`primary` is the one the screen leads with).
fn record_location_action(cx: &mut App, action: &str, screen: &str, shown: &[(&str, &str)], primary: &str) {
    let presented = tod_journey::Presented {
        actions: shown
            .iter()
            .map(|(id, label)| tod_journey::PresentedAction {
                id: (*id).to_string(),
                label: (*label).to_string(),
                primary: *id == primary,
                disabled: false,
            })
            .collect(),
        focused: Some(action.to_string()),
        notices: Vec::new(),
    };
    crate::ui::journey::record_action(
        cx,
        tod_store::conversation::Focus::Project,
        action,
        crate::ui::journey::Source::Click,
        screen,
        presented,
    );
}

const FORM_BUTTONS: &[(&str, &str)] =
    &[("loc-refresh", "Refresh"), ("loc-check", "Check"), ("loc-cancel", "Cancel"), ("loc-save", "Save")];
const LIST_BUTTONS: &[(&str, &str)] =
    &[("location-edit", "Edit"), ("location-delete", "Delete"), ("location-new", "New location")];
const CONFIRM_BUTTONS: &[(&str, &str)] = &[("loc-confirm-cancel", "Cancel"), ("loc-confirm", "Confirm")];

impl LocationsDialog {
    fn new(
        fleet: Arc<FleetStore>,
        paths: TodPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let mut this = Self {
            fleet,
            paths,
            mode: Mode::List,
            places: Vec::new(),
            usage: Vec::new(),
            editing: None,
            name: input("Name, e.g. Main repo", window, cx),
            repo: input("Workspace directory", window, cx),
            container: input("Container name", window, cx),
            image: input("Image (empty uses the default)", window, cx),
            use_worktree: true,
            runs_in: RunsIn::Machine,
            repo_on_host: false,
            fork: false,
            fork_name: String::new(),
            default_image: String::new(),
            build_open: false,
            build_base: input("Image to build on, e.g. ubuntu:24.04", window, cx),
            build_name: input("Name for the new image", window, cx),
            build_repos: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(4)
                    .placeholder("One per line: https://github.com/owner/repo [/directory]")
            }),
            building: false,
            build_result: None,
            listed: Vec::new(),
            listed_sandboxes: Vec::new(),
            listing: false,
            list_error: None,
            check: None,
            checking: false,
            generation: 0,
            busy: false,
            error: None,
        };
        this.reload(cx);
        this
    }

    /// Read the locations (and who uses them) off the UI thread.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || -> Result<_, String> {
                let places = places::list(&fleet).map_err(|err| format!("{err:#}"))?;
                let mut usage = Vec::new();
                for place in &places {
                    let impact =
                        places::impact(&fleet, &place.id, None).map_err(|err| format!("{err:#}"))?;
                    usage.push((place.id.clone(), impact.users.len()));
                }
                Ok((places, usage))
            },
            |this, result, cx| {
                match result {
                    Ok((places, usage)) => {
                        this.places = places;
                        this.usage = usage;
                    }
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            },
        );
    }

    // ---- editing -------------------------------------------------------

    fn begin_edit(&mut self, place: Option<FilesPlace>, window: &mut Window, cx: &mut Context<Self>) {
        let draft = place.clone().unwrap_or_else(|| FilesPlace {
            use_worktree: true,
            ..Default::default()
        });
        let dev = draft.dev_container.clone();
        self.runs_in = match &dev {
            Some(dev) if dev.sandbox => RunsIn::Sandbox,
            Some(_) => RunsIn::DevContainer,
            None => RunsIn::Machine,
        };
        let dev = dev.unwrap_or_default();
        self.repo_on_host = dev.repo_on_host;
        self.use_worktree = draft.use_worktree;
        let (fork, fork_name, image) = match &dev.sandbox_from {
            SandboxFrom::Fork(name) => (true, name.clone(), String::new()),
            SandboxFrom::Image(image) => (false, String::new(), image.clone()),
        };
        self.fork = fork;
        self.fork_name = fork_name;
        let set = |input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut Context<Self>| {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        };
        set(&self.name, draft.name.clone(), window, cx);
        set(&self.repo, draft.repo().unwrap_or_default().to_string(), window, cx);
        set(&self.container, dev.container().unwrap_or_default().to_string(), window, cx);
        set(&self.image, image, window, cx);
        self.editing = place;
        self.error = None;
        self.build_open = false;
        self.build_result = None;
        self.check = None;
        self.listed.clear();
        self.listed_sandboxes.clear();
        self.list_error = None;
        self.default_image = sandboxes::account_settings(self.fleet.paths().root()).1;
        self.mode = Mode::Edit;
        if self.runs_in != RunsIn::Machine {
            self.refresh_list(cx);
        }
        cx.notify();
    }

    /// The location the form describes.
    fn draft(&self, cx: &App) -> FilesPlace {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let repo = text(&self.repo);
        let dev_container = match self.runs_in {
            RunsIn::Machine => None,
            RunsIn::DevContainer => Some(DevContainerSetting {
                container: Some(text(&self.container)).filter(|c| !c.is_empty()),
                repo_on_host: self.repo_on_host,
                ..Default::default()
            }),
            RunsIn::Sandbox => Some(DevContainerSetting {
                sandbox: true,
                sandbox_from: if self.fork {
                    SandboxFrom::Fork(self.fork_name.trim().to_string())
                } else {
                    SandboxFrom::Image(text(&self.image))
                },
                ..Default::default()
            }),
        };
        FilesPlace {
            id: self.editing.as_ref().map(|p| p.id.clone()).unwrap_or_default(),
            name: text(&self.name),
            repo: Some(repo).filter(|r| !r.is_empty()),
            use_worktree: self.use_worktree && self.runs_in != RunsIn::Sandbox,
            dev_container,
        }
        .normalized()
    }

    fn set_runs_in(&mut self, runs_in: RunsIn, cx: &mut Context<Self>) {
        if self.runs_in == runs_in {
            return;
        }
        self.runs_in = runs_in;
        self.listed.clear();
        self.listed_sandboxes.clear();
        self.list_error = None;
        self.check = None;
        if runs_in != RunsIn::Machine {
            self.refresh_list(cx);
        }
        cx.notify();
    }

    /// `docker ps`, or the workspace's sandboxes, off the UI thread.
    fn refresh_list(&mut self, cx: &mut Context<Self>) {
        let sandbox = self.runs_in == RunsIn::Sandbox;
        self.listing = true;
        self.list_error = None;
        cx.notify();
        let root = self.fleet.paths().root().to_path_buf();
        off_thread(
            cx,
            move || {
                if sandbox {
                    sandboxes::list(&root).map(|l| (Vec::new(), l)).map_err(|e| format!("{e:#}"))
                } else {
                    devcontainer::list_running().map(|l| (l, Vec::new())).map_err(|e| format!("{e:#}"))
                }
            },
            move |this, result, cx| {
                if (this.runs_in == RunsIn::Sandbox) != sandbox {
                    return;
                }
                this.listing = false;
                match result {
                    Ok((containers, listed)) => {
                        this.listed = if sandbox {
                            listed.iter().map(sandbox_choice).collect()
                        } else {
                            containers
                        };
                        this.listed_sandboxes = listed;
                    }
                    Err(err) => this.list_error = Some(err),
                }
                cx.notify();
            },
        );
    }

    /// Check where launches would run, off the UI thread (this talks to
    /// Docker or wakes a sandbox).
    fn run_check(&mut self, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        self.generation += 1;
        let generation = self.generation;
        self.checking = true;
        self.check = None;
        cx.notify();
        off_thread(
            cx,
            move || check_place(&draft),
            move |this, result, cx| {
                if this.generation != generation {
                    return;
                }
                this.checking = false;
                this.check = Some(result);
                cx.notify();
            },
        );
    }

    fn choose_listed(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chosen) = self.listed.get(index) else {
            return;
        };
        let name = chosen.name.clone();
        if self.runs_in == RunsIn::Sandbox {
            self.fork = true;
            self.fork_name = name;
        } else {
            self.container.update(cx, |input, cx| input.set_value(name, window, cx));
        }
        self.check = None;
        cx.notify();
    }

    // ---- saving and deleting -------------------------------------------

    /// Check the form, then store it, asking first when nodes use the
    /// location (and removing what the change would leave behind).
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        record_location_action(cx, "locations_save", "locations_edit", FORM_BUTTONS, "loc-save");
        let draft = self.draft(cx);
        let id = draft.id.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || -> Result<PlaceImpact, String> {
                let others = places::list(&fleet).map_err(|err| format!("{err:#}"))?;
                if let Some(problem) = draft_problem(&draft, &others) {
                    return Err(problem);
                }
                if id.is_empty() {
                    return Ok(PlaceImpact::default());
                }
                places::impact(&fleet, &id, Some(&draft)).map_err(|err| format!("{err:#}"))
            },
            {
                let draft = self.draft(cx);
                move |this, result: Result<PlaceImpact, String>, cx| {
                    this.busy = false;
                    match result {
                        Err(err) => this.error = Some(err),
                        Ok(impact) if impact.is_empty() => this.write_save(draft, cx),
                        Ok(impact) => {
                            this.mode = Mode::Confirm(Pending { change: Change::Save(draft), impact })
                        }
                    }
                    cx.notify();
                }
            },
        );
    }

    fn write_save(&mut self, place: FilesPlace, cx: &mut Context<Self>) {
        self.busy = true;
        cx.notify();
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || places::save(&fleet, &place).map_err(|err| format!("{err:#}")),
            |this, result, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        this.mode = Mode::List;
                        this.reload(cx);
                    }
                    Err(err) => {
                        // Back to the form, which still holds the edit.
                        this.mode = Mode::Edit;
                        this.error = Some(err);
                    }
                }
                cx.notify();
            },
        );
    }

    fn ask_delete(&mut self, place: FilesPlace, cx: &mut Context<Self>) {
        record_location_action(cx, "locations_delete", "locations_list", LIST_BUTTONS, "location-new");
        let fleet = self.fleet.clone();
        let id = place.id.clone();
        self.error = None;
        off_thread(
            cx,
            move || places::impact(&fleet, &id, None).map_err(|err| format!("{err:#}")),
            move |this, result, cx| {
                match result {
                    Ok(impact) => {
                        this.mode = Mode::Confirm(Pending { change: Change::Delete(place), impact })
                    }
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            },
        );
    }

    fn write_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.busy = true;
        cx.notify();
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || places::delete(&fleet, &id).map_err(|err| format!("{err:#}")),
            |this, result, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.mode = Mode::List;
                        this.reload(cx);
                    }
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            },
        );
    }

    /// The user said yes to the pending change: remove the worktrees and
    /// sandboxes it leaves behind (in the shared dialog), then make it.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Mode::Confirm(pending) = std::mem::replace(&mut self.mode, Mode::List) else {
            return;
        };
        let Pending { change, impact } = pending;
        record_location_action(
            cx,
            match &change {
                Change::Save(_) => "locations_confirm_save",
                Change::Delete(_) => "locations_confirm_delete",
            },
            "locations_confirm",
            CONFIRM_BUTTONS,
            "loc-confirm",
        );
        let (title, intro, label) = match &change {
            Change::Save(place) => (
                "Change this location?",
                format!(
                    "{} now works differently. These were made from the old settings, and are \
                     removed first (each branch is pushed before it goes).",
                    place.name
                ),
                "Remove and change",
            ),
            Change::Delete(place) => (
                "Delete this location?",
                format!(
                    "These were made from {}, and are removed first (each branch is pushed \
                     before it goes).",
                    place.name
                ),
                "Remove and delete",
            ),
        };
        let this = cx.entity();
        // The dialog is shown by the shell's window; the form stays behind it
        // until the change is made.
        self.mode = Mode::Edit;
        if matches!(change, Change::Delete(_)) {
            self.mode = Mode::List;
        }
        confirm_files_change(
            window,
            cx,
            self.fleet.clone(),
            self.paths.clone(),
            impact.locations,
            title,
            intro,
            label,
            Box::new(move |_, cx| {
                this.update(cx, |this, cx| match change {
                    Change::Save(place) => this.write_save(place, cx),
                    Change::Delete(place) => this.write_delete(place.id, cx),
                });
            }),
        );
    }

    // ---- rendering -----------------------------------------------------

    fn label(&self, text: &'static str, cx: &App) -> impl IntoElement {
        div().text_xs().text_color(cx.theme().muted_foreground).child(text)
    }

    fn choice(
        &self,
        id: &'static str,
        label: &'static str,
        selected: bool,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Button {
        let button = Button::new(id).label(label).compact();
        let button = if selected { button.primary() } else { button.outline() };
        button.on_click(cx.listener(move |this, _, window, cx| on(this, window, cx)))
    }

    fn render_list(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let mut rows = v_flex().gap_1();
        for (index, place) in self.places.iter().enumerate() {
            let users = self
                .usage
                .iter()
                .find(|(id, _)| *id == place.id)
                .map(|(_, n)| *n)
                .unwrap_or(0);
            let mut detail = place.describe();
            if let Some(repo) = place.repo() {
                detail = format!("{detail} · {repo}");
            }
            let note = match (users, place.incomplete()) {
                (_, Some(reason)) => format!("Not usable yet: {reason}"),
                (0, None) => "Not used".to_string(),
                (1, None) => "Used by 1 node".to_string(),
                (n, None) => format!("Used by {n} nodes"),
            };
            let (edit, delete) = (place.clone(), place.clone());
            rows = rows.child(
                h_flex()
                    .id(ElementId::NamedInteger("location-row".into(), index as u64))
                    .gap_2()
                    .p_2()
                    .items_center()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(selectable_text(
                                SharedString::from(format!("location-name-{index}")),
                                place.name.clone(),
                                window,
                                cx,
                            )))
                            .child(div().text_xs().text_color(muted).child(selectable_text(
                                SharedString::from(format!("location-detail-{index}")),
                                detail,
                                window,
                                cx,
                            )))
                            .child(div().text_xs().text_color(muted).child(note)),
                    )
                    .child(
                        Button::new(ElementId::NamedInteger("location-edit".into(), index as u64))
                            .label("Edit")
                            .outline()
                            .compact()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_edit(Some(edit.clone()), window, cx);
                            })),
                    )
                    .child(
                        Button::new(ElementId::NamedInteger("location-delete".into(), index as u64))
                            .label("Delete")
                            .outline()
                            .compact()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.ask_delete(delete.clone(), cx);
                            })),
                    ),
            );
        }
        v_flex()
            .gap_3()
            .child(div().text_xs().text_color(muted).child(
                "A location says where a node's files are: a directory on this machine, in a dev \
                 container, or in cloud sandboxes. A node's Files capability picks one.",
            ))
            .when(self.places.is_empty(), |el| {
                el.child(div().text_sm().text_color(muted).child("No locations yet."))
            })
            .child(rows)
            .when_some(self.error.clone(), |el, err| el.child(self.error_text(err, window, cx)))
            .child(
                h_flex().justify_end().child(
                    Button::new("location-new")
                        .label("New location")
                        .primary()
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| this.begin_edit(None, window, cx))),
                ),
            )
            .into_any_element()
    }

    fn error_text(&self, text: String, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().text_xs().text_color(cx.theme().danger).child(selectable_text(
            "locations-error",
            text,
            window,
            cx,
        ))
    }

    /// Build the image the form describes on Blaxel (a minute or more, so off
    /// the UI thread), then make it what each node's sandbox starts from.
    fn build_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.building {
            return;
        }
        let base = self.build_base.read(cx).value().trim().to_string();
        let name = self.build_name.read(cx).value().trim().to_string();
        let repos_text = self.build_repos.read(cx).value().to_string();
        let checked = (|| -> anyhow::Result<_> {
            if base.is_empty() || base.contains(char::is_whitespace) {
                anyhow::bail!("Give the image to build on, such as ubuntu:24.04");
            }
            if sandboxes::runs_as_is(&base) {
                anyhow::bail!("Build on a plain base image such as ubuntu:24.04, not on one built for Blaxel");
            }
            sandboxes::validate_name(&name)?;
            let repos = sandboxes::parse_bake_repos(&repos_text)?;
            if repos.is_empty() {
                anyhow::bail!("Add at least one repository (one https:// URL per line)");
            }
            Ok(repos)
        })();
        let repos = match checked {
            Ok(repos) => repos,
            Err(err) => {
                self.build_result = Some(Err(format!("{err:#}")));
                cx.notify();
                return;
            }
        };
        self.building = true;
        self.build_result = None;
        cx.notify();
        let root = self.fleet.paths().root().to_path_buf();
        let dirs = repos.iter().map(|r| r.dir.clone()).collect::<Vec<_>>().join(", ");
        cx.spawn_in(window, async move |this, cx| {
            let built = cx
                .background_spawn(async move {
                    sandboxes::Sandboxes::load(&root)?.bake(&base, &name, true, &repos, false, &mut |_| {})
                })
                .await;
            let result = this.update_in(cx, |this, window, cx| {
                this.building = false;
                match built {
                    Ok(image) => {
                        this.image.update(cx, |input, cx| input.set_value(image.clone(), window, cx));
                        this.fork = false;
                        this.check = None;
                        this.build_open = false;
                        this.build_result = Some(Ok(format!(
                            "Built {image}, now this location's image. Cloned: {dirs}. Use one of \
                             those as the workspace directory, then Save."
                        )));
                    }
                    Err(err) => this.build_result = Some(Err(format!("{err:#}"))),
                }
                cx.notify();
            });
            if let Err(err) = result {
                tracing::error!("the Locations dialog closed before the image build ended: {err:#}");
            }
        })
        .detach();
    }

    fn render_image_build(&self, muted: gpui::Hsla, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let danger = cx.theme().danger;
        let open = self.build_open;
        let toggle = Button::new("loc-build-toggle")
            .label(if open { "Close image builder" } else { "Image with repositories…" })
            .outline()
            .compact()
            .on_click(cx.listener(|this, _, _, cx| {
                this.build_open = !this.build_open;
                cx.notify();
            }));
        let form = open.then(|| {
            v_flex()
                .gap_2()
                .pl_3()
                .child(div().text_xs().text_color(muted).child(
                    "Clones each repository into one image, so nodes start with them already there \
                     (a sandbox can work in any of them; list source it depends on too). Public \
                     https:// URLs only: credentials in a URL would be kept in the image.",
                ))
                .child(v_flex().gap_1().child(self.label("Built on", cx)).child(Input::new(&self.build_base)))
                .child(v_flex().gap_1().child(self.label("Image name", cx)).child(Input::new(&self.build_name)))
                .child(
                    v_flex()
                        .gap_1()
                        .child(self.label("Repositories", cx))
                        .child(Textarea::new(&self.build_repos).w_full().h(px(96.))),
                )
                .child(
                    Button::new("loc-build-run")
                        .label(if self.building { "Building…" } else { "Build image" })
                        .outline()
                        .compact()
                        .disabled(self.building)
                        .on_click(cx.listener(|this, _, window, cx| this.build_image(window, cx))),
                )
                .when(self.building, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Building on Blaxel; a minute or more. You can keep working."),
                    )
                })
        });
        let result = self.build_result.clone().map(|result| {
            let (text, color) = match result {
                Ok(text) => (text, muted),
                Err(text) => (text, danger),
            };
            div().text_xs().text_color(color).child(selectable_text("locations-build-result", text, window, cx))
        });
        v_flex().gap_2().child(toggle).when_some(form, |el, f| el.child(f)).when_some(result, |el, r| el.child(r))
    }

    fn render_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let foreground = cx.theme().foreground;
        let runs_in = self.runs_in;
        let field = |label: &'static str, input: &Entity<InputState>, cx: &App| {
            v_flex().gap_1().child(self.label(label, cx)).child(Input::new(input))
        };

        let mut list = v_flex().gap_0p5();
        let chosen_container = self.container.read(cx).value().trim().to_string();
        for (index, item) in self.listed.iter().enumerate() {
            let chosen = if runs_in == RunsIn::Sandbox {
                self.fork && self.fork_name == item.name
            } else {
                !chosen_container.is_empty()
                    && (chosen_container == item.name
                        || (chosen_container.len() >= 12 && item.id.starts_with(&chosen_container)))
            };
            let mut detail = item.image.clone();
            if let Some(folder) = &item.local_folder {
                detail = format!("{detail} · {folder}");
            }
            list = list.child(
                h_flex()
                    .id(ElementId::NamedInteger("location-listed".into(), index as u64))
                    .gap_2()
                    .px_1()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().list_hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_listed(index, window, cx);
                    }))
                    .child(div().text_sm().text_color(foreground).child(if chosen { "●" } else { "○" }))
                    .child(div().text_sm().text_color(foreground).flex_shrink_0().child(item.name.clone()))
                    .when(item.is_dev_container(), |el| {
                        el.child(div().text_xs().text_color(muted).child("dev container"))
                    })
                    .child(div().min_w_0().text_xs().text_color(muted).child(detail)),
            );
        }
        if self.listed.is_empty() && !self.listing && self.list_error.is_none() {
            list = list.child(div().text_xs().text_color(muted).child(if runs_in == RunsIn::Sandbox {
                "No sandboxes in the workspace to fork."
            } else {
                "No running containers. Start the dev container, then Refresh."
            }));
        }

        let status = if self.checking {
            Some((muted, "Checking (this may wake a sandbox)…".to_string()))
        } else {
            match &self.check {
                Some(Ok(text)) => Some((muted, text.clone())),
                Some(Err(err)) => Some((danger, err.clone())),
                None => None,
            }
        };
        let creating = self.editing.is_none();

        v_flex()
            .gap_3()
            .child(field("Name", &self.name, cx))
            .child(field(
                match runs_in {
                    RunsIn::Machine => "Workspace directory (on this machine)",
                    RunsIn::DevContainer if self.repo_on_host => "Workspace directory (on this machine)",
                    RunsIn::DevContainer => "Workspace directory (inside the dev container)",
                    RunsIn::Sandbox => "Workspace directory (inside the sandbox)",
                },
                &self.repo,
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(self.label("Runs in", cx))
                    .child(self.choice("loc-runs-machine", "This machine", runs_in == RunsIn::Machine, cx, |this, _, cx| {
                        this.set_runs_in(RunsIn::Machine, cx)
                    }))
                    .child(self.choice("loc-runs-container", "Dev container", runs_in == RunsIn::DevContainer, cx, |this, _, cx| {
                        this.set_runs_in(RunsIn::DevContainer, cx)
                    }))
                    .child(self.choice("loc-runs-sandbox", "Cloud sandbox", runs_in == RunsIn::Sandbox, cx, |this, _, cx| {
                        this.set_runs_in(RunsIn::Sandbox, cx)
                    })),
            )
            .when(runs_in != RunsIn::Sandbox, |el| {
                el.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(self.label("Worktrees", cx))
                        .child(self.choice("loc-worktree-on", "A worktree for each node", self.use_worktree, cx, |this, _, cx| {
                            this.use_worktree = true;
                            this.check = None;
                            cx.notify();
                        }))
                        .child(self.choice("loc-worktree-off", "The directory itself", !self.use_worktree, cx, |this, _, cx| {
                            this.use_worktree = false;
                            this.check = None;
                            cx.notify();
                        })),
                )
            })
            .when(runs_in == RunsIn::DevContainer, |el| {
                el.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(self.label("Repository", cx))
                        .child(self.choice("loc-repo-inside", "Inside the container", !self.repo_on_host, cx, |this, _, cx| {
                            this.repo_on_host = false;
                            this.check = None;
                            cx.notify();
                        }))
                        .child(self.choice("loc-repo-mounted", "On this machine, mounted", self.repo_on_host, cx, |this, _, cx| {
                            this.repo_on_host = true;
                            this.check = None;
                            cx.notify();
                        })),
                )
                .child(field("Container", &self.container, cx))
            })
            .when(runs_in == RunsIn::Sandbox, |el| {
                el.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(self.label("Each node's sandbox starts from", cx))
                        .child(self.choice("loc-from-image", "An image", !self.fork, cx, |this, _, cx| {
                            this.fork = false;
                            this.check = None;
                            cx.notify();
                        }))
                        .child(self.choice("loc-from-fork", "A fork of a sandbox", self.fork, cx, |this, _, cx| {
                            this.fork = true;
                            this.check = None;
                            cx.notify();
                        })),
                )
                .when(!self.fork, |el| {
                    el.child(field("Image", &self.image, cx))
                        .child(div().text_xs().text_color(muted).child(format!(
                            "Empty uses {} (Settings → Cloud sandboxes). The image must hold the \
                             repository.",
                            self.default_image
                        )))
                        .child(self.render_image_build(muted, window, cx))
                })
                .when(self.fork, |el| {
                    el.child(div().text_xs().text_color(muted).child(if self.fork_name.is_empty() {
                        "None chosen: pick a sandbox below".to_string()
                    } else {
                        format!("Fork of {}", self.fork_name)
                    }))
                })
            })
            .when(runs_in != RunsIn::Machine && (runs_in == RunsIn::DevContainer || self.fork), |el| {
                el.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(self.label(
                            if runs_in == RunsIn::Sandbox { "Sandboxes to fork" } else { "Running containers" },
                            cx,
                        ))
                        .child(
                            Button::new("loc-refresh")
                                .label(if self.listing { "Listing…" } else { "Refresh" })
                                .outline()
                                .compact()
                                .disabled(self.listing)
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_list(cx))),
                        ),
                )
                .when_some(self.list_error.clone(), |el, err| el.child(self.error_text(err, window, cx)))
                .child(list)
            })
            .when_some(status, |el, (color, text)| {
                el.child(div().text_xs().text_color(color).child(selectable_text(
                    "locations-check",
                    text,
                    window,
                    cx,
                )))
            })
            .when_some(self.error.clone(), |el, err| el.child(self.error_text(err, window, cx)))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("loc-check")
                            .label("Check")
                            .outline()
                            .compact()
                            .disabled(self.checking)
                            .on_click(cx.listener(|this, _, _, cx| this.run_check(cx))),
                    )
                    .child(
                        Button::new("loc-cancel")
                            .label("Cancel")
                            .outline()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mode = Mode::List;
                                this.error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("loc-save")
                            .label(if creating { "Create" } else { "Save" })
                            .primary()
                            .compact()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let Mode::Confirm(pending) = &self.mode else {
            return div().into_any_element();
        };
        let (heading, action, name) = match &pending.change {
            Change::Save(place) => ("Change a location nodes use", "Change it", place.name.clone()),
            Change::Delete(place) => ("Delete a location nodes use", "Delete it", place.name.clone()),
        };
        let deleting = matches!(pending.change, Change::Delete(_));
        let users = pending.impact.users.clone();
        let locations = pending.impact.locations.len();
        let consequence = match (deleting, locations) {
            (true, 0) => "Nodes that use it will have no location until one is chosen.".to_string(),
            (true, n) => format!(
                "Nodes that use it will have no location until one is chosen. {n} worktree{} or \
                 sandbox{} made from it will be removed first, each branch pushed before it goes.",
                if n == 1 { "" } else { "s" },
                if n == 1 { "" } else { "es" },
            ),
            (false, 0) => "Their worktrees and sandboxes stay as they are.".to_string(),
            (false, n) => format!(
                "{n} worktree{} or sandbox{} made from the old settings will be removed first, \
                 each branch pushed before it goes.",
                if n == 1 { "" } else { "s" },
                if n == 1 { "" } else { "es" },
            ),
        };
        let mut used_by = v_flex().gap_0p5();
        for (index, user) in users.iter().enumerate() {
            used_by = used_by.child(div().text_sm().child(selectable_text(
                SharedString::from(format!("location-user-{index}")),
                user.node_title.clone(),
                window,
                cx,
            )));
        }
        v_flex()
            .gap_3()
            .child(div().text_sm().child(format!("{heading}: {name}")))
            .child(div().text_xs().text_color(muted).child(if users.is_empty() {
                "No node uses it, but worktrees or sandboxes made from it remain.".to_string()
            } else {
                format!(
                    "{} node{} picked it (nodes below them inherit it):",
                    users.len(),
                    if users.len() == 1 { "" } else { "s" }
                )
            }))
            .child(used_by)
            .child(div().text_xs().text_color(muted).child(consequence))
            .when_some(self.error.clone(), |el, err| el.child(self.error_text(err, window, cx)))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("loc-confirm-cancel")
                            .label("Cancel")
                            .outline()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                let was_edit = matches!(
                                    &this.mode,
                                    Mode::Confirm(Pending { change: Change::Save(_), .. })
                                );
                                this.mode = if was_edit { Mode::Edit } else { Mode::List };
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("loc-confirm")
                            .label(action)
                            .danger()
                            .compact()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for LocationsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The form can outgrow the window (the image builder is long), so the
        // body scrolls rather than push its buttons off screen.
        let max_height = (window.viewport_size().height - px(200.)).max(px(240.));
        let body = match &self.mode {
            Mode::List => self.render_list(window, cx),
            Mode::Edit => self.render_edit(window, cx),
            Mode::Confirm(_) => self.render_confirm(window, cx),
        };
        div().id("locations-body").max_h(max_height).overflow_y_scroll().child(body)
    }
}

/// Why `draft` cannot be stored, against the stored locations.
fn draft_problem(draft: &FilesPlace, others: &[FilesPlace]) -> Option<String> {
    draft.problem(others)
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

/// Whether launches from `place` would work: what is missing, or where they
/// would run. Talks to Docker or Blaxel.
fn check_place(place: &FilesPlace) -> Result<String, String> {
    if let Some(reason) = place.incomplete() {
        return Err(reason);
    }
    let repo = place.repo().map(str::to_string);
    match &place.dev_container {
        None => {
            let repo = repo.ok_or("Set a workspace directory")?;
            let path = std::path::Path::new(&repo);
            if !path.is_dir() {
                return Err(format!("{repo} is not a directory on this machine"));
            }
            if place.use_worktree && !path.join(".git").exists() {
                return Err(format!(
                    "{repo} is not a git repository, which a worktree needs"
                ));
            }
            Ok(format!("Directory {repo} on this machine"))
        }
        Some(dev) if dev.sandbox => {
            let target = match &dev.sandbox_from {
                SandboxFrom::Fork(name) if !name.trim().is_empty() => name.trim().to_string(),
                from => {
                    return Ok(format!(
                        "Each node gets a sandbox of its own from {}, made when it needs its \
                         files. The image must hold the repository at {}.",
                        from.describe(),
                        repo.unwrap_or_default()
                    ));
                }
            };
            check_repo_in_sandbox(&target, repo.as_deref())
                .map(|found| format!("The sandbox each node forks: {found}"))
        }
        Some(dev) => {
            let container = dev.container().ok_or("Choose a dev container")?.to_string();
            if dev.repo_on_host {
                let host_dir = std::path::PathBuf::from(repo.unwrap_or_default());
                devcontainer::resolve_directory(&container, &host_dir, None)
                    .map(|(info, dir)| match info.remote_user {
                        Some(user) => format!("Runs in {dir} in {} as {user}", info.name),
                        None => format!("Runs in {dir} in {}", info.name),
                    })
                    .map_err(|err| format!("{err:#}"))
            } else {
                check_repo_in_container(&container, repo.as_deref(), place.use_worktree)
            }
        }
    }
}

fn check_repo_in_sandbox(sandbox: &str, repo: Option<&str>) -> Result<String, String> {
    let exec = sandboxes::SandboxExec::new(sandbox);
    let Some(repo) = repo else {
        exec.output("/", "true", &[]).map_err(|err| format!("{err:#}"))?;
        return Ok(format!("Sandbox {sandbox} is ready. Set the workspace directory to a path in it."));
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
    directory_verdict(repo, &place, "", git, is_dir, false)
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

/// The container is running and `repo` (a path in it) is a directory there:
/// a git repository when worktrees are on. Talks to Docker.
fn check_repo_in_container(
    container: &str,
    repo: Option<&str>,
    use_worktree: bool,
) -> Result<String, String> {
    let exec = devcontainer::ContainerExec::connect(container).map_err(|err| format!("{err:#}"))?;
    let as_user = exec.user.as_deref().map(|user| format!(" as {user}")).unwrap_or_default();
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
