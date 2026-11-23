#[cfg(feature = "live")]
mod live;
mod workspace;

use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};
use tod_palette::{Effect, Palette, ResultRow};
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

#[derive(Clone)]
struct App {
    workspace: Signal<Workspace>,
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
        self.palette.set(Some(
            self.workspace
                .with(|workspace| workspace.palette(invoked, actions)),
        ));
        self.open.set(true);
    }

    fn current(&self) -> Option<Palette> {
        let mut palette = self.palette.get()?;
        palette.set_query(self.draft.get());
        Some(palette)
    }

    fn focus_list(&self) {
        if let Some(list) = self.list.borrow().as_ref() {
            list.focus();
        }
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
        let mut workspace = self.workspace.get();
        match workspace.apply(effect) {
            Ok(()) => {
                if !navigation {
                    self.save_frequency(&mut workspace);
                }
                self.workspace.set(workspace);
                self.open.set(false);
                if navigation {
                    self.focus_list();
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
        let mut workspace = self.workspace.get();
        if let Err(error) = workspace.apply(Effect::Open(id)) {
            workspace.message = error;
        }
        self.workspace.set(workspace);
        self.focus_list();
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

fn mount(
    cx: &mut WindowContext,
    workspace: Workspace,
    usage_path: Option<PathBuf>,
    #[cfg(feature = "live")] backend: Option<live::Backend>,
) -> (App, ViewHandle) {
    let app = App {
        workspace: cx.ui.signal(workspace),
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
    let shortcut = app.clone();
    let search = app.clone();
    let context = app.clone();
    let chips = app.workspace.clone();
    let chip_app = app.clone();
    let detail = app.workspace.clone();
    let message = app.workspace.clone();
    let mode = app.workspace.with(Workspace::label);
    let view = cx.render(column().takes_unfocused_keys().w_full().h_full().p(23.).gap(14.)
        .bg(rgb(BACKGROUND)).text_color(rgb(INK)).text_size(14.)
        .on_event(move |event| {
            if event.phase != EventPhase::Bubble
                && let InputEvent::KeyDown { key: Key::Character(key), modifiers, repeat: false } = &event.event
                && modifiers.primary_shortcut() && key.eq_ignore_ascii_case("k") && !shortcut.open.get() {
                shortcut.open(None, false);
                event.prevent_default();
                event.stop_propagation();
            }
        })
        .child(row().items_center().gap(10.)
            .child(text("tod").text_size(23.))
            .child(text(mode).text_color(rgb(MUTED)).grow())
            .child(control("Actions…  ⇧Enter").on_click(move || context.open(None, true)))
            .child(control(if cfg!(target_os = "macos") { "Search  ⌘K" } else { "Search  Ctrl+K" })
                .id("open-palette").on_click(move || search.open(None, false))))
        .child(switch(move || chips.get(), move |workspace, _| {
            row().gap(6.).children(workspace.collections().map(|record| {
                let id = record.item.id.clone();
                let app = chip_app.clone();
                control(record.item.title.clone()).bg(rgb(if workspace.collection == id { ACCENT } else { PANEL }))
                    .on_click(move || app.open_item(id.clone()))
            }))
        }))
        .child(row().gap(18.).grow()
            .child(task_list(&app))
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
        .child(text_signal(move || message.with(|workspace| workspace.message.clone())).text_color(rgb(MUTED)))
        .child(palette_view(&app)));
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/agent/scratchpad/tod-zgx/palette-usage.json");
    let mut smoke = false;
    let mut data_root: Option<PathBuf> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--smoke-test" {
            smoke = true;
        } else if arg == "--data-root" {
            data_root = Some(args.next().ok_or("--data-root needs a directory")?.into());
        } else if arg == "--state" {
            path = args.next().ok_or("--state needs a file")?.into();
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
    let smoke_result = Rc::new(std::cell::Cell::new(false));
    let result = smoke_result.clone();
    Application::new()
        .window(WindowOptions {
            title: "tod · ZGX".into(),
            width: 930.,
            height: 640.,
            min_size: Some((690., 550.)),
            ..Default::default()
        })
        .run(move |cx| {
            let (app, view) = mount(
                cx,
                workspace.clone(),
                (!smoke).then(|| path.clone()),
                #[cfg(feature = "live")]
                backend.clone(),
            );
            if smoke {
                let window = cx.window.clone();
                let ui = cx.ui.downgrade();
                let result = result.clone();
                cx.tasks.spawn(async move {
                    zgx::timer::sleep(Duration::from_millis(350)).await;
                    let passed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let ui = &mut ui.upgrade().expect("native ZGX window");
                        if app.workspace.with(Workspace::label) == "Command palette demo" {
                            smoke_test(ui, &app, &view);
                        } else {
                            live_smoke_test(ui, &app, &view);
                        }
                    }))
                    .is_ok();
                    result.set(passed);
                    if passed {
                        println!("Native ZGX palette flow passed");
                    }
                    window.close();
                });
            }
        })?;
    if smoke && !smoke_result.get() {
        return Err("Native ZGX palette smoke test failed".into());
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
