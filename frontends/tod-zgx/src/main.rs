mod collection;
mod lifecycle;
mod lifecycle_editor;
#[cfg(feature = "live")]
mod live;
#[cfg(feature = "live")]
mod workflow_assets;
mod workflow_runs;
mod workspace;

use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};
use tod_palette::{Context, Effect, Palette, ResultRow};
use workspace::Workspace;
use zgx::{
    compose::{NodeHandle, ViewHandle, prelude::*},
    input::{EventContext, EventPhase, InputEvent, Key, Modifiers, PointerButton},
    reactive::Signal,
};
use zgx_desktop::{Application, WindowContext, WindowOptions};

const INK: u32 = 0xe9eef5;
const MUTED: u32 = 0xa2afc1;
const BACKGROUND: u32 = 0x111722;
const PANEL: u32 = 0x1a2231;
const LINE: u32 = 0x344257;
const ACCENT: u32 = 0x284a6e;
const ROW_HEIGHT: f32 = 44.;
const HEADING_HEIGHT: f32 = 26.;
const RESULTS_HEIGHT: f32 = 319.;

#[derive(Clone, Copy, PartialEq)]
enum Surface {
    Workspace,
    Collections,
    Lifecycle,
    Workflows,
    WorkflowRun,
    #[cfg(feature = "live")]
    WorkflowActions,
}

#[derive(Clone)]
struct App {
    workspace: Signal<Workspace>,
    collections: Option<collection::Panels>,
    lifecycle: lifecycle::Panel,
    workflow_editor: lifecycle_editor::Editor,
    workflow_runs: workflow_runs::Panel,
    #[cfg(feature = "live")]
    workflow_assets: workflow_assets::Panel,
    #[cfg(feature = "live")]
    draft_generation: Rc<RefCell<Option<(String, u64)>>>,
    #[cfg(feature = "live")]
    workflow_busy: Signal<bool>,
    #[cfg(feature = "live")]
    outline: Option<collection::OutlinePanel>,
    surface: Signal<Surface>,
    palette: Signal<Option<Palette>>,
    open: Signal<bool>,
    draft: Signal<String>,
    scroll: Signal<f32>,
    error: Signal<String>,
    list: Rc<RefCell<Option<NodeHandle>>>,
    input: Rc<RefCell<Option<NodeHandle>>>,
    usage_path: Option<PathBuf>,
    #[cfg(feature = "live")]
    backend: Option<live::Backend>,
    #[cfg(feature = "live")]
    tasks: zgx_desktop::TaskSpawner,
}

impl App {
    fn open(&self, invoked: Option<&str>, actions: bool) {
        self.draft.set(String::new());
        self.error.set(String::new());
        self.scroll.set(0.);
        let extra = self
            .collections
            .as_ref()
            .map_or_else(Vec::new, |panels| panels.search_items().collect());
        let context = if self.surface.get() == Surface::Collections {
            self.collections.as_ref().map(|_| Context {
                focused: Some(collection::ID.into()),
                ..Default::default()
            })
        } else {
            None
        };
        #[cfg(feature = "live")]
        let context = context.or_else(|| {
            if self.surface.get() != Surface::Collections {
                return None;
            }
            self.outline.as_ref()?.palette_context()
        });
        self.palette
            .set(Some(self.workspace.with(|workspace| {
                workspace.palette(invoked, actions, extra, context)
            })));
        self.open.set(true);
    }

    fn current(&self) -> Option<Palette> {
        let mut palette = self.palette.get()?;
        palette.set_query(self.draft.get());
        Some(palette)
    }

    fn focus_list(&self) {
        if let Some(list) = self.list.borrow().as_ref() {
            if !list.focus() {
                let later = list.clone();
                list.after_layout(move || {
                    later.focus();
                });
            }
        }
    }

    fn show_collections(&self) {
        if let Some(panels) = &self.collections {
            self.surface.set(Surface::Collections);
            panels.focus();
        }
        #[cfg(feature = "live")]
        if let Some(outline) = &self.outline {
            if outline.ready() {
                self.surface.set(Surface::Collections);
                outline.focus();
            } else if self.surface.get() != Surface::Collections {
                let id = self
                    .workspace
                    .with(|workspace| workspace.collection.clone());
                self.open_saved_outline(&id);
            }
        }
    }

    #[cfg(feature = "live")]
    fn open_saved_outline(&self, id: &str) -> bool {
        let (Some(outline), Some(backend)) = (&self.outline, &self.backend) else {
            return false;
        };
        let title = self.workspace.with(|workspace| {
            workspace
                .record(id)
                .filter(|record| record.item.kind == tod_palette::Kind::Collection)
                .map(|record| record.item.title.clone())
        });
        let Some(title) = title else {
            return false;
        };
        let mut workspace = self.workspace.get();
        if let Err(error) = workspace.apply(Effect::Open(id.into())) {
            self.error.set(error);
            return true;
        }
        self.workspace.set(workspace);
        self.open.set(false);
        let request = outline.begin(title);
        self.surface.set(Surface::Collections);
        let task = backend.load_outline(id.into());
        let app = self.clone();
        let outline = outline.clone();
        self.tasks.spawn(async move {
            let result = task
                .await
                .unwrap_or_else(|error| Err(format!("Outline worker failed. {error}")));
            if outline.finish(request, result)
                && app.surface.get() == Surface::Collections
                && !app.open.get()
            {
                outline.focus();
            }
        });
        true
    }

    fn show_workspace(&self) {
        self.surface.set(Surface::Workspace);
        self.focus_list();
    }

    fn show_lifecycle(&self) {
        if self.surface.get() == Surface::Lifecycle || self.lifecycle.busy.get() {
            return;
        }
        #[cfg(feature = "live")]
        let outlined = if self.surface.get() == Surface::Collections {
            self.outline
                .as_ref()
                .and_then(|outline| outline.palette_context()?.focused)
        } else {
            None
        };
        #[cfg(not(feature = "live"))]
        let outlined: Option<String> = None;
        #[cfg(feature = "live")]
        let live = self.backend.is_some();
        #[cfg(not(feature = "live"))]
        let live = false;
        let snapshot = self.workspace.with(|workspace| {
            let id = outlined.as_deref().unwrap_or(&workspace.detail);
            workspace
                .record(id)
                .filter(|record| {
                    use tod_palette::Kind::*;
                    record.item.kind == Task
                        || (live && matches!(record.item.kind, Node | ProductDefinition))
                })
                .map(|record| (id.to_owned(), record.item.title.clone()))
        });
        self.surface.set(Surface::Lifecycle);
        #[cfg(feature = "live")]
        if let (Some(backend), Some((id, title))) = (&self.backend, snapshot.as_ref()) {
            self.lifecycle.open(lifecycle::Data {
                node: Some(id.clone()),
                title: title.clone(),
                ..Default::default()
            });
            self.lifecycle.busy.set(true);
            self.lifecycle
                .notice
                .set("Loading lifecycle details…".into());
            let task = backend.load_lifecycle(id.clone());
            let panel = self.lifecycle.clone();
            self.tasks.spawn(async move {
                let result = task.await;
                panel.busy.set(false);
                match result {
                    Ok(Ok(data)) => panel.open(data),
                    Ok(Err(error)) => {
                        panel.notice.set(error);
                    }
                    Err(error) => {
                        panel
                            .notice
                            .set(format!("Could not load lifecycle details. {error}"));
                    }
                }
            });
            return;
        }
        let mut data = lifecycle::Data::default();
        if let Some((_, title)) = snapshot {
            data.title = title;
            data.phase = "review".into();
        }
        self.lifecycle.open(data);
    }

    fn show_workflows(&self) {
        self.surface.set(Surface::Workflows);
        self.workflow_event(lifecycle_editor::Event::Refresh);
    }

    fn workflow_event(&self, event: lifecycle_editor::Event) {
        #[cfg(feature = "live")]
        if matches!(event, lifecycle_editor::Event::ManageActions) && self.backend.is_some() {
            self.surface.set(Surface::WorkflowActions);
            self.workflow_assets_command(workflow_assets::Command::Load);
            return;
        }
        #[cfg(feature = "live")]
        if let Some(backend) = &self.backend {
            if self.workflow_busy.get() {
                return;
            }
            let submitted = match &event {
                lifecycle_editor::Event::Save { definition, .. }
                | lifecycle_editor::Event::Publish { definition, .. } => Some(definition.clone()),
                _ => None,
            };
            let generation = submitted.as_ref().and_then(|definition| {
                self.draft_generation
                    .borrow()
                    .as_ref()
                    .filter(|(id, _)| id == &definition.revision.id)
                    .map(|(_, generation)| *generation)
            });
            self.workflow_busy.set(true);
            self.workflow_editor.set_busy(true);
            let task = backend.workflow_catalog(event, generation);
            let app = self.clone();
            self.tasks.spawn(async move {
                let result = task.await;
                app.workflow_busy.set(false);
                app.workflow_editor.set_busy(false);
                match result {
                    Ok(Ok(update)) => {
                        if let Some(draft) = update.draft {
                            app.draft_generation.replace(Some((
                                draft.definition.revision.id.clone(),
                                draft.generation,
                            )));
                            if let Some(submitted) = submitted {
                                app.workflow_editor.accept_saved(
                                    draft.title,
                                    draft.definition,
                                    submitted,
                                );
                            } else {
                                app.workflow_editor.open(draft.title, draft.definition);
                            }
                        }
                        app.workflow_editor.update_library(update.library);
                        app.workflow_editor.set_actions(update.actions);
                        app.workflow_editor.status(update.notice);
                    }
                    Ok(Err(error)) => app.workflow_editor.status(error),
                    Err(error) => app
                        .workflow_editor
                        .status(format!("Workflow catalog failed. {error}")),
                }
            });
            return;
        }
        if !matches!(event, lifecycle_editor::Event::Refresh) {
            self.workflow_editor
                .status("Open a live TOD data root to save workflow drafts.".into());
        }
    }

    #[cfg(feature = "live")]
    fn workflow_assets_command(&self, command: workflow_assets::Command) {
        let Some(backend) = &self.backend else { return };
        if self.workflow_assets.busy.get() {
            return;
        }
        let panel = self.workflow_assets.clone();
        let editor = self.workflow_editor.clone();
        panel.busy.set(true);
        panel.status.set(String::new());
        let task = backend.workflow_assets(command);
        self.tasks.spawn(async move {
            let result = task.await;
            panel.busy.set(false);
            match result {
                Ok(Ok(update)) => {
                    editor.set_actions(
                        update
                            .library
                            .iter()
                            .map(|asset| (asset.spec.title.clone(), asset.revision.clone()))
                            .collect(),
                    );
                    panel.library.set(update.library);
                    if let Some(history) = update.history {
                        panel.history.set(history);
                    }
                    if let Some(saved) = update.saved {
                        panel.status.set(format!(
                            "Published action revision {}.",
                            saved.revision.number
                        ));
                        panel.open_published(saved);
                    }
                }
                Ok(Err(error)) => {
                    panel.status.set(error);
                }
                Err(error) => {
                    panel.status.set(format!("Agent actions failed. {error}"));
                }
            }
        });
    }

    fn show_workflow_run(&self) {
        if self.workflow_runs.busy.get() {
            return;
        }
        let task = self
            .lifecycle
            .data
            .with(|data| data.node.clone().map(|id| (id, data.title.clone())));
        self.surface.set(Surface::WorkflowRun);
        self.workflow_runs.open(task);
        self.workflow_run_command(workflow_runs::Command::Load);
    }

    fn workflow_run_command(&self, command: workflow_runs::Command) {
        #[cfg(feature = "live")]
        if let (Some(backend), Some((task, title))) = (&self.backend, self.workflow_runs.task.get())
        {
            if self.workflow_runs.busy.get() {
                return;
            }
            let panel = self.workflow_runs.clone();
            panel.busy.set(true);
            panel.notice.set(String::new());
            let expected = task.clone();
            let operation = backend.workflow_run(task, title, command);
            self.tasks.spawn(async move {
                let outcome = operation.await;
                panel.busy.set(false);
                if !panel
                    .task
                    .with(|task| task.as_ref().is_some_and(|(id, _)| id == &expected))
                {
                    return;
                }
                match outcome {
                    Ok(Ok((snapshot, notice))) => {
                        panel.state.set(snapshot);
                        panel.notice.set(notice);
                    }
                    Ok(Err(error)) => {
                        panel.notice.set(error);
                    }
                    Err(error) => {
                        panel.notice.set(format!("Workflow action failed. {error}"));
                    }
                }
            });
            return;
        }
        let _ = command;
    }

    fn save_lifecycle_agent(
        &self,
        key: &'static str,
        skills: Option<String>,
        model: String,
        effort: String,
    ) {
        #[cfg(feature = "live")]
        if let (Some(backend), Some(id)) = (
            &self.backend,
            self.lifecycle.data.with(|data| data.node.clone()),
        ) {
            if self.lifecycle.busy.get() {
                return;
            }
            let panel = self.lifecycle.clone();
            panel.busy.set(true);
            panel.notice.set("Saving…".into());
            let task = backend.save_lifecycle_agent(id, key, skills, model, effort);
            self.tasks.spawn(async move {
                let result = task.await;
                panel.busy.set(false);
                match result {
                    Ok(Ok(data)) => {
                        panel.data.set(data);
                        panel.saved();
                    }
                    Ok(Err(error)) => {
                        panel.notice.set(error);
                    }
                    Err(error) => {
                        panel
                            .notice
                            .set(format!("Could not save lifecycle settings. {error}"));
                    }
                }
            });
            return;
        }
        let _ = (key, skills, model, effort);
    }

    fn escape(&self) {
        if let Some(mut palette) = self.current()
            && palette.back()
        {
            self.show_page(palette);
        } else {
            self.open.set(false);
        }
    }

    fn show_page(&self, palette: Palette) {
        self.draft.set(palette.query().to_owned());
        self.error.set(String::new());
        self.scroll.set(0.);
        self.reveal(&palette);
        self.palette.set(Some(palette));
        if let Some(input) = self.input.borrow().as_ref() {
            input.focus();
        }
    }

    fn reveal(&self, palette: &Palette) {
        let Some(index) = palette.active() else {
            return;
        };
        let headings = palette
            .sections()
            .partition_point(|section| section.start <= index);
        let bottom = (index + 1) as f32 * ROW_HEIGHT + headings as f32 * HEADING_HEIGHT;
        let top = bottom - ROW_HEIGHT;
        let offset = self.scroll.get();
        if top < offset {
            self.scroll.set((top - HEADING_HEIGHT).max(0.));
        }
        if bottom > offset + RESULTS_HEIGHT {
            self.scroll.set(bottom - RESULTS_HEIGHT);
        }
    }

    fn commit(&self, mut palette: Palette, index: usize) {
        let Some(effect) = palette.activate(index) else {
            self.show_page(palette);
            return;
        };
        if let Some(panels) = &self.collections {
            match &effect {
                Effect::Open(id) if panels.contains(id) => {
                    self.open.set(false);
                    self.surface.set(Surface::Collections);
                    panels.open_item(id);
                    return;
                }
                Effect::Run {
                    command: "edit-collection",
                    targets,
                } if targets.as_slice() == [collection::ID] => {
                    let mut workspace = self.workspace.get();
                    workspace.record_use("edit-collection");
                    self.save_frequency(&mut workspace);
                    self.workspace.set(workspace);
                    self.open.set(false);
                    self.surface.set(Surface::Collections);
                    panels.open_config();
                    return;
                }
                _ => {}
            }
        }
        #[cfg(feature = "live")]
        if let Effect::Open(id) = &effect
            && self.open_saved_outline(id)
        {
            return;
        }
        #[cfg(feature = "live")]
        if let Some((command, task)) = self
            .backend
            .as_ref()
            .and_then(|backend| backend.run(&effect))
        {
            self.open.set(false);
            self.workspace
                .update(|workspace| workspace.message = "Requesting runner action…".into());
            let app = self.clone();
            self.tasks.spawn(async move {
                let result = task.await;
                let mut workspace = app.workspace.get();
                workspace.message = match result {
                    Ok(Ok(message)) => {
                        workspace.record_use(command);
                        message
                    }
                    Ok(Err(message)) => format!("Runner action failed. {message}"),
                    Err(error) => format!("Runner action worker failed. {error}"),
                };
                app.save_frequency(&mut workspace);
                app.workspace.set(workspace);
            });
            return;
        }
        let navigation = matches!(effect, Effect::Open(_));
        let opens_workspace = navigation
            || matches!(
                &effect,
                Effect::Run {
                    command: "shortcuts" | "conversation",
                    ..
                }
            );
        let mut workspace = self.workspace.get();
        match workspace.apply(effect) {
            Ok(()) => {
                if !navigation {
                    self.save_frequency(&mut workspace);
                }
                self.workspace.set(workspace);
                self.open.set(false);
                if opens_workspace {
                    self.show_workspace();
                }
            }
            Err(error) => {
                self.error.set(error);
            }
        }
    }

    fn save_frequency(&self, workspace: &mut Workspace) {
        if let Some(path) = &self.usage_path
            && let Err(error) = save_usage(path, &workspace.frequency)
        {
            workspace.message = format!(
                "{} Could not save command frequency. {error}",
                workspace.message
            );
        }
    }

    fn actions(&self) {
        if let Some(mut palette) = self.current()
            && palette.actions()
        {
            self.show_page(palette);
        }
    }

    fn palette_key(&self, event: &mut EventContext) {
        if event.phase == EventPhase::Bubble {
            return;
        }
        let InputEvent::KeyDown {
            key,
            modifiers,
            repeat,
        } = &event.event
        else {
            return;
        };
        let Some(mut palette) = self.current() else {
            return;
        };
        match key {
            Key::ArrowUp | Key::ArrowDown => {
                palette.move_highlight(*key == Key::ArrowDown, modifiers.alt);
                self.reveal(&palette);
                self.palette.set(Some(palette));
            }
            Key::Enter if !repeat => {
                if modifiers.shift {
                    self.actions();
                } else if let Some(index) = palette.active() {
                    self.commit(palette, index);
                }
            }
            Key::Escape if !repeat => self.escape(),
            _ => return,
        }
        event.prevent_default();
        event.stop_propagation();
    }

    fn workspace_key(&self, event: &mut EventContext) {
        if self.open.get() || event.phase == EventPhase::Bubble {
            return;
        }
        let InputEvent::KeyDown {
            key,
            modifiers,
            repeat,
        } = &event.event
        else {
            return;
        };
        match key {
            Key::ArrowUp | Key::ArrowDown => {
                self.workspace.update(|workspace| {
                    workspace.move_cursor(*key == Key::ArrowDown, modifiers.alt)
                });
            }
            Key::Space if !repeat => {
                self.workspace.update(|workspace| {
                    let id = workspace.cursor.clone();
                    if workspace
                        .visible_tasks()
                        .iter()
                        .any(|row| row.item.id == id)
                    {
                        workspace.toggle_mark(&id);
                    }
                });
            }
            Key::Enter if modifiers.shift && !repeat => self.open(None, true),
            Key::Enter if !repeat => {
                let id = self.workspace.with(|workspace| workspace.cursor.clone());
                self.open_item(id);
            }
            _ => return,
        }
        event.prevent_default();
        event.stop_propagation();
    }

    fn open_item(&self, id: String) {
        #[cfg(feature = "live")]
        if self.open_saved_outline(&id) {
            return;
        }
        let mut workspace = self.workspace.get();
        if let Err(error) = workspace.apply(Effect::Open(id)) {
            workspace.message = error;
        }
        self.workspace.set(workspace);
        self.show_workspace();
    }
}

fn control(label: impl Into<String>) -> View {
    button()
        .h(30.)
        .px(10.)
        .rounded(5.)
        .bg(rgb(PANEL))
        .hover(|style| style.bg(rgb(ACCENT)))
        .child(text(label.into()))
}

fn result(app: &App, index: usize, row: ResultRow, active: bool) -> View {
    let click = app.clone();
    button()
        .id(format!("palette-result-{index}"))
        .accessible_label(format!("{} {} {}", row.title, row.detail, row.kind))
        .h(ROW_HEIGHT)
        .w_full()
        .px(9.)
        .rounded(5.)
        .bg(rgb(if active { ACCENT } else { PANEL }))
        .hover(|style| style.bg(rgb(ACCENT)))
        .child(
            zgx::compose::row()
                .w_full()
                .gap(12.)
                .items_center()
                .child(
                    column()
                        .grow()
                        .min_w(0.)
                        .child(text(row.title))
                        .child(text(row.detail).text_size(11.).text_color(rgb(MUTED))),
                )
                .child(text(row.kind).text_size(11.).text_color(rgb(MUTED))),
        )
        .on_click(move || {
            if let Some(palette) = click.current() {
                click.commit(palette, index);
            }
        })
}

fn results(app: App) -> View {
    let current = app.clone();
    scroll(app.scroll.clone())
        .id("palette-results")
        .w_full()
        .h(RESULTS_HEIGHT)
        .child(switch_or_none(
            move || current.current(),
            move |palette, _| {
                let palette = palette?;
                let mut list = column().w_full();
                for (section_index, section) in palette.sections().iter().enumerate() {
                    if palette.sections().len() > 1 || palette.title() == "Search tod" {
                        list = list.child(
                            text(section.heading.clone())
                                .id(format!("palette-section-{section_index}"))
                                .h(HEADING_HEIGHT)
                                .px(8.)
                                .text_size(12.)
                                .text_color(rgb(MUTED)),
                        );
                    } else {
                        list = list.child(div().h(HEADING_HEIGHT));
                    }
                    let end = palette
                        .sections()
                        .get(section_index + 1)
                        .map_or(palette.rows().len(), |next| next.start);
                    for index in section.start..end {
                        list = list.child(result(
                            &app,
                            index,
                            palette.rows()[index].clone(),
                            palette.active() == Some(index),
                        ));
                    }
                }
                if palette.rows().is_empty() {
                    list = list.child(text("No matches").p(10.).text_color(rgb(MUTED)));
                }
                Some(list)
            },
        ))
}

fn palette_view(app: &App) -> View {
    let title = app.clone();
    let active = app.clone();
    let input = app.input.clone();
    let reset_scroll = app.scroll.clone();
    let keys = app.clone();
    let back = app.clone();
    let actions = app.clone();
    let show_actions = app.clone();
    let error = app.error.clone();
    modal("Command palette", app.open.clone())
        .id("command-palette")
        .w(600.)
        .p(16.)
        .gap(8.)
        .rounded(10.)
        .bg(rgb(PANEL))
        .border(1.)
        .border_color(rgb(LINE))
        .text_color(rgb(INK))
        .dismiss_on_backdrop(false)
        .on_event(move |event| keys.palette_key(event))
        .child(
            row()
                .items_center()
                .gap(10.)
                .child(
                    text_signal(move || {
                        title
                            .current()
                            .map_or(String::new(), |palette| palette.title())
                    })
                    .grow()
                    .text_size(19.),
                )
                .child(
                    control("Esc")
                        .id("palette-back")
                        .focusable(false)
                        .on_click(move || back.escape()),
                ),
        )
        .child(
            text_input("Search tod", app.draft.clone())
                .id("palette-query")
                .w_full()
                .h(36.)
                .px(8.)
                .rounded(5.)
                .bg(rgb(BACKGROUND))
                .border(1.)
                .border_color(rgb(LINE))
                .placeholder("Search commands, items, and collections")
                .active_descendant(move || {
                    active
                        .current()?
                        .active()
                        .map(|index| format!("palette-result-{index}"))
                })
                .on_mount(move |node| {
                    input.replace(Some(node));
                })
                .on_event(move |event| {
                    if matches!(
                        event.event,
                        InputEvent::Text(_)
                            | InputEvent::ImeCommit(_)
                            | InputEvent::KeyDown {
                                key: Key::Backspace | Key::Delete,
                                ..
                            }
                    ) {
                        reset_scroll.set(0.);
                    }
                }),
        )
        .child(results(app.clone()))
        .child(
            text_signal(move || error.get())
                .text_size(12.)
                .text_color(rgb(0xffb3a5)),
        )
        .child(
            row()
                .items_center()
                .gap(8.)
                .child(
                    text("↑ ↓ Move   ·   Option/Alt ↑ ↓ Section   ·   Enter Open or run")
                        .grow()
                        .text_size(12.)
                        .text_color(rgb(MUTED)),
                )
                .child(switch_or_none(
                    move || {
                        show_actions.current().is_some_and(|palette| {
                            palette
                                .active()
                                .is_some_and(|index| palette.rows()[index].offers_actions)
                        })
                    },
                    move |show, _| {
                        let actions = actions.clone();
                        show.then(|| {
                            control("Actions…  ⇧Enter")
                                .id("palette-actions")
                                .on_click(move || actions.actions())
                        })
                    },
                )),
        )
}

fn task_list(app: &App) -> View {
    let mount = app.list.clone();
    let keys = app.clone();
    let rows = app.workspace.clone();
    let selection = app.workspace.clone();
    let row_app = app.clone();
    column()
        .id("task-list")
        .accessible_label("Collection rows")
        .focusable(true)
        .grow()
        .gap(2.)
        .active_descendant(move || {
            Some(format!(
                "task-{}",
                selection.with(|workspace| workspace.cursor.clone())
            ))
        })
        .on_mount(move |node| {
            mount.replace(Some(node));
        })
        .on_event(move |event| keys.workspace_key(event))
        .child(switch(
            move || rows.get(),
            move |workspace, _| {
                let mut list = column().gap(3.).w_full();
                let mut last_group = None;
                for record in workspace.visible_tasks() {
                    let id = record.item.id;
                    if last_group != record.item.scope {
                        if let Some(group) = &record.item.scope {
                            list = list.child(
                                text(group.clone())
                                    .p(5.)
                                    .text_size(12.)
                                    .text_color(rgb(MUTED)),
                            );
                        }
                        last_group = record.item.scope;
                    }
                    let mark_id = id.clone();
                    let open_id = id.clone();
                    let explicit_id = id.clone();
                    let open = row_app.clone();
                    let mark = row_app.clone();
                    let actions = row_app.clone();
                    list = list.child(
                        row()
                            .id(format!("task-{id}"))
                            .h(34.)
                            .gap(6.)
                            .px(3.)
                            .w_full()
                            .items_center()
                            .rounded(5.)
                            .bg(rgb(if workspace.cursor == id {
                                ACCENT
                            } else {
                                PANEL
                            }))
                            .on_event(move |event| {
                                if event.phase != EventPhase::Bubble
                                    && matches!(
                                        event.event,
                                        InputEvent::PointerDown {
                                            button: PointerButton::Secondary,
                                            ..
                                        }
                                    )
                                {
                                    actions.open(Some(&explicit_id), true);
                                    event.prevent_default();
                                    event.stop_propagation();
                                }
                            })
                            .child(
                                control(if workspace.selected.contains(&id) {
                                    "☑"
                                } else {
                                    "☐"
                                })
                                .id(format!("mark-{id}"))
                                .accessible_label(format!("Mark {}", record.item.title))
                                .on_click(move || {
                                    mark.workspace
                                        .update(|workspace| workspace.toggle_mark(&mark_id));
                                    mark.focus_list();
                                }),
                            )
                            .child(
                                button()
                                    .grow()
                                    .id(format!("open-{id}"))
                                    .h(30.)
                                    .child(text(record.item.title))
                                    .on_click(move || open.open_item(open_id.clone())),
                            )
                            .child(text(record.status).text_size(12.).text_color(rgb(MUTED))),
                    );
                }
                if list_is_empty(&workspace) {
                    list = list.child(text("No tasks in this collection.").p(7.));
                }
                list
            },
        ))
}

fn list_is_empty(workspace: &Workspace) -> bool {
    workspace.visible_tasks().is_empty()
}

fn workspace_view(app: &App) -> View {
    let chips = app.workspace.clone();
    let chip_app = app.clone();
    let detail = app.workspace.clone();
    column().id("task-workspace").w_full().h_full().gap(14.)
        .child(switch(move || chips.get(), move |workspace, _| {
            row().gap(6.).children(workspace.collections().map(|record| {
                let id = record.item.id.clone();
                let app = chip_app.clone();
                control(record.item.title.clone()).bg(rgb(if workspace.collection == id { ACCENT } else { PANEL }))
                    .on_click(move || app.open_item(id.clone()))
            }))
        }))
        .child(row().gap(18.).grow()
            .child(task_list(app))
            .child(switch(move || detail.get(), move |workspace, _| {
                let (title, body) = if workspace.detail == "shortcuts" {
                    ("Keyboard shortcuts".into(), "⌘/Ctrl+K searches across tod. Arrow keys move by row. Option/Alt+↑/↓ moves by section. Enter opens or runs. Shift+Enter opens actions. Esc goes back.".into())
                } else if workspace.detail == "conversation" {
                    ("Agent conversation".into(), workspace.conversation.join(" · "))
                } else {
                    workspace.record(&workspace.detail).map_or((String::new(), String::new()), |record| {
                        (record.item.title.clone(), record.description.clone())
                    })
                };
                column().w(290.).p(14.).gap(8.).rounded(7.).bg(rgb(PANEL))
                    .child(text(title).text_size(20.).text_wrap(true))
                    .child(text(body).text_wrap(true).text_color(rgb(MUTED)))
            })))
}

fn surface_style(active: &Signal<Surface>, surface: Surface) -> Styles {
    if active.get() == surface {
        Styles::new()
    } else {
        Styles::new().hidden()
    }
}

fn surface_tabs(app: &App) -> View {
    let tasks = app.clone();
    let collections = app.clone();
    let active_tasks = app.surface.clone();
    let active_collections = app.surface.clone();
    row()
        .gap(5.)
        .child(
            control("Tasks")
                .id("show-tasks")
                .reactive_style(move || {
                    Styles::new().bg(rgb(if active_tasks.get() == Surface::Workspace {
                        ACCENT
                    } else {
                        PANEL
                    }))
                })
                .on_click(move || tasks.show_workspace()),
        )
        .child(
            control("Collection panels")
                .id("show-collections")
                .reactive_style(move || {
                    Styles::new().bg(rgb(if active_collections.get() == Surface::Collections {
                        ACCENT
                    } else {
                        PANEL
                    }))
                })
                .on_click(move || collections.show_collections()),
        )
}

fn mount(
    cx: &mut WindowContext,
    workspace: Workspace,
    usage_path: Option<PathBuf>,
    collection_data: Option<&collection::Data>,
    #[cfg(feature = "live")] backend: Option<live::Backend>,
) -> (App, ViewHandle) {
    #[cfg(feature = "live")]
    let outline = (backend.is_some() && workspace.collections().next().is_some())
        .then(|| collection::OutlinePanel::new(cx));
    let app = App {
        workspace: cx.ui.signal(workspace),
        collections: collection_data.map(|data| collection::Panels::new(cx, data)),
        lifecycle: lifecycle::Panel::new(cx),
        workflow_editor: lifecycle_editor::Editor::new(cx),
        workflow_runs: workflow_runs::Panel::new(cx),
        #[cfg(feature = "live")]
        workflow_assets: workflow_assets::Panel::new(cx),
        #[cfg(feature = "live")]
        draft_generation: Default::default(),
        #[cfg(feature = "live")]
        workflow_busy: cx.ui.signal(false),
        #[cfg(feature = "live")]
        outline,
        surface: cx.ui.signal(Surface::Workspace),
        palette: cx.ui.signal(None),
        open: cx.ui.signal(false),
        draft: cx.ui.signal(String::new()),
        scroll: cx.ui.signal(0.),
        error: cx.ui.signal(String::new()),
        list: Default::default(),
        input: Default::default(),
        usage_path,
        #[cfg(feature = "live")]
        backend,
        #[cfg(feature = "live")]
        tasks: cx.tasks.clone(),
    };
    app.workflow_editor
        .set_starter(tod_core::lifecycle::workflow::builtin::review_workflow());
    let workflow_surface = app.surface.clone();
    let workflow_actions = app.clone();
    let workflow_tab = app.clone();
    let run_surface = app.surface.clone();
    let shortcut = app.clone();
    let search = app.clone();
    let context = app.clone();
    let message = app.workspace.clone();
    let tasks_screen = app.surface.clone();
    let lifecycle_screen = app.surface.clone();
    let lifecycle_tab = app.clone();
    let mut screens = overlay()
        .grow()
        .min_h(0.)
        .child(
            workspace_view(&app)
                .reactive_style(move || surface_style(&tasks_screen, Surface::Workspace)),
        )
        .child(
            app.lifecycle
                .view(&app)
                .reactive_style(move || surface_style(&lifecycle_screen, Surface::Lifecycle)),
        )
        .child(
            app.workflow_editor
                .view(move |event| workflow_actions.workflow_event(event))
                .reactive_style(move || surface_style(&workflow_surface, Surface::Workflows)),
        )
        .child(
            app.workflow_runs
                .view(&app)
                .reactive_style(move || surface_style(&run_surface, Surface::WorkflowRun)),
        );
    #[cfg(feature = "live")]
    {
        let actions = app.clone();
        let back = app.clone();
        let active = app.surface.clone();
        screens = screens.child(
            app.workflow_assets
                .view(
                    move |action| actions.workflow_assets_command(action),
                    move || {
                        back.surface.set(Surface::Workflows);
                    },
                )
                .reactive_style(move || surface_style(&active, Surface::WorkflowActions)),
        );
    }
    let mut header = row()
        .items_center()
        .gap(10.)
        .child(text("tod").text_size(23.));
    #[cfg(feature = "live")]
    let live_screen = app.outline.as_ref().map(|outline| outline.view(cx));
    #[cfg(not(feature = "live"))]
    let live_screen: Option<View> = None;
    let collection_screen = app
        .collections
        .as_ref()
        .map(|panels| panels.view(cx))
        .or(live_screen);
    if let Some(collection_screen) = collection_screen {
        let collections_screen = app.surface.clone();
        screens = screens.child(
            collection_screen
                .reactive_style(move || surface_style(&collections_screen, Surface::Collections)),
        );
        header = header.child(surface_tabs(&app));
    } else {
        let tasks = app.clone();
        header = header.child(
            control("Tasks")
                .id("show-tasks")
                .on_click(move || tasks.show_workspace()),
        );
    }
    let active = app.surface.clone();
    header = header.child(
        control("Lifecycle")
            .id("show-lifecycle")
            .reactive_style(move || {
                Styles::new().bg(rgb(if active.get() == Surface::Lifecycle {
                    ACCENT
                } else {
                    PANEL
                }))
            })
            .on_click(move || lifecycle_tab.show_lifecycle()),
    );
    let active_workflows = app.surface.clone();
    header = header.child(
        control("Workflows")
            .id("show-workflows")
            .reactive_style(move || {
                Styles::new().bg(rgb(if active_workflows.get() == Surface::Workflows {
                    ACCENT
                } else {
                    PANEL
                }))
            })
            .on_click(move || workflow_tab.show_workflows()),
    );
    let view = cx.render(
        column()
            .takes_unfocused_keys()
            .w_full()
            .h_full()
            .p(16.)
            .gap(12.)
            .bg(rgb(BACKGROUND))
            .text_color(rgb(INK))
            .text_size(14.)
            .on_event(move |event| {
                if event.phase != EventPhase::Bubble
                    && let InputEvent::KeyDown {
                        key: Key::Character(key),
                        modifiers,
                        repeat: false,
                    } = &event.event
                    && modifiers.primary_shortcut()
                    && key.eq_ignore_ascii_case("k")
                    && !shortcut.open.get()
                {
                    shortcut.open(None, false);
                    event.prevent_default();
                    event.stop_propagation();
                }
            })
            .child(
                header
                    .child(div().grow())
                    .child(control("Actions…  ⇧Enter").on_click(move || context.open(None, true)))
                    .child(
                        control(if cfg!(target_os = "macos") {
                            "Search  ⌘K"
                        } else {
                            "Search  Ctrl+K"
                        })
                        .id("open-palette")
                        .on_click(move || search.open(None, false)),
                    ),
            )
            .child(screens)
            .child(switch_or_none(
                move || message.with(|workspace| workspace.message.clone()),
                |message, _| {
                    (!message.is_empty())
                        .then(|| text(message).text_wrap(true).text_color(rgb(MUTED)))
                },
            ))
            .child(palette_view(&app)),
    );
    (app, view)
}

fn load_usage(path: &Path) -> Result<BTreeMap<String, u32>, Box<dyn std::error::Error>> {
    match std::fs::read(path) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}

fn save_usage(
    path: &Path,
    usage: &BTreeMap<String, u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let next = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&next, serde_json::to_vec(usage)?)?;
    std::fs::rename(next, path)?;
    Ok(())
}

#[cfg(feature = "agent-socket")]
fn start_socket(cx: &mut WindowContext, view: ViewHandle) -> std::io::Result<()> {
    use zgx_script::{ScriptHost, inspect};
    let host = ScriptHost::inline(&cx.ui.runtime, "tod ZGX", "");
    let session = inspect::Session::new(&cx.ui, view, &host, None);
    let tasks = cx.tasks.clone();
    let server = inspect::serve(session, "tod ZGX", move |serve| tasks.spawn(serve))?;
    eprintln!("ZGX inspection socket at {}", server.path().display());
    cx.on_closed(move || drop(server));
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/agent/scratchpad/tod-zgx/palette-usage.json");
    let mut smoke = false;
    let mut data_root: Option<PathBuf> = None;
    let mut collection_state = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--smoke-test" {
            smoke = true;
        } else if arg == "--data-root" {
            data_root = Some(args.next().ok_or("--data-root needs a directory")?.into());
        } else if arg == "--state" {
            path = args.next().ok_or("--state needs a file")?.into();
        } else if arg == "--collection-state" {
            collection_state = Some(args.next().ok_or("--collection-state needs a file")?.into());
        } else {
            return Err(format!("Unknown argument {}", arg.to_string_lossy()).into());
        }
    }
    let frequency = if smoke {
        BTreeMap::new()
    } else {
        load_usage(&path)?
    };
    #[cfg(feature = "live")]
    let backend = data_root.as_deref().map(live::Backend::open).transpose()?;
    #[cfg(feature = "live")]
    let workspace = match &backend {
        Some(backend) => backend.load(frequency)?,
        None => Workspace::demo(frequency),
    };
    #[cfg(not(feature = "live"))]
    let workspace = if let Some(root) = data_root {
        return Err(format!(
            "Opening {} requires building with --features live",
            root.display()
        )
        .into());
    } else {
        Workspace::demo(frequency)
    };
    let demo = workspace.label() == "Command palette demo";
    let collection_data = demo
        .then(|| collection::Data::load(collection_state, smoke))
        .transpose()?;
    #[cfg(feature = "agent-socket")]
    let socket = zgx_script::inspect::asked();
    #[cfg(feature = "agent-socket")]
    let socket_error = Rc::new(RefCell::new(None));
    #[cfg(feature = "agent-socket")]
    let socket_result = socket_error.clone();
    #[cfg(feature = "agent-socket")]
    if socket {
        zgx_script::inspect::prepare();
    }
    let smoke_result = Rc::new(std::cell::Cell::new(false));
    let result = smoke_result.clone();
    Application::new()
        .window(WindowOptions {
            title: "tod · ZGX".into(),
            width: if demo { 1160. } else { 930. },
            height: if demo { 750. } else { 640. },
            min_size: Some((if demo { 960. } else { 690. }, 550.)),
            ..Default::default()
        })
        .run(move |cx| {
            let (app, view) = mount(
                cx,
                workspace.clone(),
                (!smoke).then(|| path.clone()),
                collection_data.as_ref(),
                #[cfg(feature = "live")]
                backend.clone(),
            );
            #[cfg(feature = "agent-socket")]
            if socket && let Err(error) = start_socket(cx, view.clone()) {
                socket_result.replace(Some(error));
                cx.window.close();
                return;
            }
            if smoke {
                let window = cx.window.clone();
                let ui = cx.ui.downgrade();
                let result = result.clone();
                cx.tasks.spawn(async move {
                    zgx::timer::sleep(Duration::from_millis(350)).await;
                    let passed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let ui = &mut ui.upgrade().expect("native ZGX window");
                        if demo {
                            smoke_test(ui, &app, &view);
                            combined_smoke_test(ui, &app, &view);
                        } else {
                            live_smoke_test(ui, &app, &view);
                        }
                    }))
                    .is_ok();
                    result.set(passed);
                    if passed {
                        println!(
                            "Native ZGX {} checks passed",
                            if demo {
                                "palette and collection"
                            } else {
                                "live palette"
                            }
                        );
                    }
                    window.close();
                });
            }
        })?;
    #[cfg(feature = "agent-socket")]
    if let Some(error) = socket_error.take() {
        return Err(error.into());
    }
    if smoke && !smoke_result.get() {
        return Err("Native ZGX smoke test failed".into());
    }
    Ok(())
}

fn live_smoke_test(ui: &mut zgx::widgets::Ui, app: &App, view: &ViewHandle) {
    let item = app.workspace.with(|workspace| {
        workspace
            .visible_tasks()
            .into_iter()
            .next()
            .expect("seed a live fixture")
            .item
    });
    app.open(None, false);
    ui.prepare_frame();
    assert_eq!(ui.input.focused(), view.find("palette-query"));
    ui.dispatch(InputEvent::Text(item.title));
    ui.prepare_frame();
    assert_eq!(
        app.current().unwrap().rows()[0].effect,
        Effect::Open(item.id.clone())
    );
    press(ui, Key::Enter, Modifiers::default());
    assert_eq!(
        app.workspace.with(|workspace| workspace.detail.clone()),
        item.id
    );
}

fn smoke_test(ui: &mut zgx::widgets::Ui, app: &App, view: &ViewHandle) {
    let primary = Modifiers {
        meta: cfg!(target_os = "macos"),
        control: !cfg!(target_os = "macos"),
        ..Default::default()
    };
    press(ui, Key::Character("k".into()), primary);
    assert!(app.open.get());
    assert_eq!(ui.input.focused(), view.find("palette-query"));
    let first = app.current().unwrap();
    assert_eq!(first.sections()[0].heading, "Fix sync retries");
    press(
        ui,
        Key::ArrowDown,
        Modifiers {
            alt: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.current().unwrap().active(),
        Some(first.sections()[1].start)
    );
    ui.dispatch(InputEvent::Text("p rn".into()));
    ui.prepare_frame();
    let query = app.current().unwrap();
    let row = &query.rows()[query.active().unwrap()];
    assert_eq!(row.title, "Pause runner");
    assert_eq!(row.detail, "Fix sync retries");
    press(ui, Key::Enter, Modifiers::default());
    assert!(!app.open.get());
    assert_eq!(
        app.workspace
            .with(|workspace| workspace.record("sync-retries").unwrap().status.clone()),
        "Paused"
    );

    app.focus_list();
    press(ui, Key::Character("k".into()), primary);
    ui.dispatch(InputEvent::Text("nmd".into()));
    ui.prepare_frame();
    press(
        ui,
        Key::Enter,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.current().unwrap().title(),
        "Actions for Needs my decision"
    );
    press(ui, Key::Escape, Modifiers::default());
    assert_eq!(app.current().unwrap().query(), "nmd");
    assert_eq!(app.current().unwrap().rows()[0].title, "Needs my decision");
    press(ui, Key::Escape, Modifiers::default());
    assert!(!app.open.get());
    assert_eq!(ui.input.focused(), view.find("task-list"));

    press(ui, Key::Character("k".into()), primary);
    ui.dispatch(InputEvent::Text("keyboard shortcuts".into()));
    ui.prepare_frame();
    assert_eq!(app.current().unwrap().sections()[0].heading, "Everywhere");
    press(ui, Key::Escape, Modifiers::default());

    app.focus_list();
    press(ui, Key::Space, Modifiers::default());
    press(ui, Key::ArrowDown, Modifiers::default());
    press(ui, Key::Space, Modifiers::default());
    press(
        ui,
        Key::Enter,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.current().unwrap().title(),
        "Actions for 2 selected tasks"
    );
    ui.dispatch(InputEvent::Text("pause runner".into()));
    press(ui, Key::Enter, Modifiers::default());
    let chooser = app.current().unwrap();
    assert_eq!(chooser.title(), "Choose a target · Pause runner");
    assert_eq!(chooser.rows().len(), 2);
    press(ui, Key::Escape, Modifiers::default());
    assert_eq!(app.current().unwrap().query(), "pause runner");
    press(ui, Key::Escape, Modifiers::default());

    let right_click = view.find("open-webhooks").expect("visible unmarked task");
    let bounds = ui.scene.borrow().visible_bounds(right_click).unwrap();
    ui.dispatch(InputEvent::PointerDown {
        x: bounds.x + 8.,
        y: bounds.y + 5.,
        button: PointerButton::Secondary,
    });
    ui.prepare_frame();
    assert_eq!(
        app.current().unwrap().title(),
        "Actions for Trace failed webhooks"
    );
    assert_eq!(
        app.workspace.with(|workspace| workspace.detail.clone()),
        "sync-retries"
    );
    assert_eq!(app.workspace.with(|workspace| workspace.selected.len()), 2);
    press(ui, Key::Escape, Modifiers::default());
    press(ui, Key::Character("k".into()), primary);
    ui.dispatch(InputEvent::Text("pin tasks".into()));
    press(ui, Key::Enter, Modifiers::default());
    assert_eq!(app.workspace.with(|workspace| workspace.pinned.len()), 2);
    assert_eq!(
        app.workspace
            .with(|workspace| workspace.frequency.get("pin").copied()),
        Some(1)
    );
}

fn press(ui: &mut zgx::widgets::Ui, key: Key, modifiers: Modifiers) {
    ui.dispatch(InputEvent::KeyDown {
        key: key.clone(),
        modifiers,
        repeat: false,
    });
    ui.dispatch(InputEvent::KeyUp { key, modifiers });
    ui.prepare_frame();
}

fn combined_smoke_test(ui: &mut zgx::widgets::Ui, app: &App, view: &ViewHandle) {
    let primary = Modifiers {
        meta: cfg!(target_os = "macos"),
        control: !cfg!(target_os = "macos"),
        ..Default::default()
    };
    let ordinary = Modifiers::default();
    press(ui, Key::Character("k".into()), primary);
    ui.dispatch(InputEvent::Text("design work".into()));
    let palette = app.current().unwrap();
    assert_eq!(
        palette.rows()[palette.active().unwrap()].effect,
        Effect::Open(collection::ID.into())
    );
    press(ui, Key::Enter, ordinary);
    assert_eq!(ui.input.focused(), view.find("Left-collection-pane"));

    press(ui, Key::Character("k".into()), primary);
    ui.dispatch(InputEvent::Text("edit collection".into()));
    let palette = app.current().unwrap();
    assert_eq!(
        palette.rows()[palette.active().unwrap()].effect,
        Effect::Run {
            command: "edit-collection",
            targets: vec![collection::ID.into()]
        }
    );
    press(ui, Key::Enter, ordinary);
    assert_eq!(ui.input.focused(), view.find("Left-chip-strip"));
    press(ui, Key::Escape, ordinary);
    app.show_collections();
    ui.prepare_frame();
    app.collections.as_ref().unwrap().smoke_test(ui, view);
}
