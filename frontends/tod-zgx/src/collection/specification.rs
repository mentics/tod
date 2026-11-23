use super::{Slot, control, model};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use zgx::{compose::prelude::*, reactive::Signal, semantics::Live, widgets::Ui};

pub const REQUIREMENT: &str = "REQ-COLLECTION-01";
pub const IMPLEMENTATION: &str = "verify-keyboard";

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Specification {
    id: String,
    implementation: String,
    revisions: Vec<String>,
}

impl Specification {
    pub fn load(path: &Path, initial_body: &str) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let spec: Self = serde_json::from_slice(&bytes)?;
                if spec.id != REQUIREMENT
                    || spec.implementation != IMPLEMENTATION
                    || spec.revisions.is_empty()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid collection requirement history",
                    ));
                }
                Ok(spec)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self {
                id: REQUIREMENT.into(),
                implementation: IMPLEMENTATION.into(),
                revisions: vec![initial_body.into()],
            }),
            Err(error) => Err(error),
        }
    }

    pub fn path(config_path: &Path) -> PathBuf {
        config_path.with_extension("spec.json")
    }
}

#[derive(Clone)]
pub struct SpecStore {
    value: Signal<Specification>,
    path: PathBuf,
    initial_body: Arc<str>,
}

impl SpecStore {
    pub fn new(ui: &Ui, spec: Specification, path: PathBuf, initial_body: Arc<str>) -> Self {
        Self {
            value: ui.signal(spec),
            path,
            initial_body,
        }
    }

    fn save(&self, base: usize, body: String) -> Result<(), String> {
        if body.trim().is_empty() {
            return Err("Enter requirement text before saving.".into());
        }
        let (current, accepted) = self
            .write(base, body)
            .map_err(|error| format!("Could not save revision. {error}"))?;
        self.value.set(current);
        if !accepted {
            return Err("A newer revision is available. Copy your draft before cancelling if you want to keep it.".into());
        }
        Ok(())
    }

    fn write(&self, base: usize, body: String) -> io::Result<(Specification, bool)> {
        model::ensure_parent(&self.path)?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("lock"))?;
        lock.try_lock().map_err(io::Error::other)?;
        let mut latest = Specification::load(&self.path, &self.initial_body)?;
        if latest.revisions.len() != base {
            return Ok((latest, false));
        }
        if latest.revisions.last() != Some(&body) {
            latest.revisions.push(body);
            model::save_json(&self.path, &latest)?;
        }
        Ok((latest, true))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Current,
    Edit(usize),
    History,
    Revision(usize),
}

pub fn inspect(
    slot: Slot,
    item: model::Item,
    store: SpecStore,
    open_work: impl Fn(&str) + Clone + 'static,
    implementation_label: Arc<str>,
) -> View {
    component(move |cx| {
        let mode = cx.state(Mode::Current);
        let draft = cx.state(String::new());
        let error = cx.state(String::new());
        let offset = cx.state(0.);
        let select = mode.clone();
        let title = format!("{}  ·  {}", item.id, item.name);
        column()
            .grow()
            .min_h(0.)
            .gap(5.)
            .p(9.)
            .bg(rgb(0x171717))
            .rounded(6.)
            .child(
                text(title)
                    .id(slot.id("inspector-title"))
                    .font_bold()
                    .h(22.),
            )
            .child(switch(
                move || select.get(),
                move |view, _| {
                    let error = error.clone();
                    let mode = mode.clone();
                    let store = store.clone();
                    let draft = draft.clone();
                    let offset = offset.clone();
                    let open_work = open_work.clone();
                    offset.set(0.);
                    match view {
                        Mode::Edit(base) => edit_form(slot, store, mode, draft, error, base),
                        Mode::History => history(slot, store, mode, offset),
                        Mode::Current | Mode::Revision(_) => {
                            let revision = match view {
                                Mode::Revision(number) => Some(number),
                                _ => None,
                            };
                            document(
                                slot,
                                store,
                                mode,
                                draft,
                                offset,
                                revision,
                                open_work,
                                implementation_label.to_string(),
                            )
                        }
                    }
                },
            ))
    })
}

fn edit_form(
    slot: Slot,
    store: SpecStore,
    mode: Signal<Mode>,
    draft: Signal<String>,
    error: Signal<String>,
    base: usize,
) -> View {
    let save_mode = mode.clone();
    let save_error = error.clone();
    let save_text = draft.clone();
    let clear = error.clone();
    column()
        .grow()
        .min_h(0.)
        .gap(6.)
        .child(
            row()
                .gap(5.)
                .child(
                    control("Save revision")
                        .id(slot.id("spec-save"))
                        .on_click(move || {
                            match store.save(base, save_text.get()) {
                                Ok(()) => save_mode.set(Mode::Current),
                                Err(message) => save_error.set(message),
                            };
                        }),
                )
                .child(
                    control("Cancel")
                        .id(slot.id("spec-cancel"))
                        .on_click(move || {
                            clear.set(String::new());
                            mode.set(Mode::Current);
                        }),
                ),
        )
        .child(switch_or_none(
            move || error.get(),
            |message, _| {
                (!message.is_empty()).then(|| {
                    text(message)
                        .text_wrap(true)
                        .text_color(rgb(0xffb4a5))
                        .live(Live::Polite)
                })
            },
        ))
        .child(
            text_area("Requirement text", draft)
                .id(slot.id("spec-editor"))
                .grow()
                .min_h(65.)
                .w_full()
                .p(8.)
                .text_size(13.)
                .text_wrap(true)
                .bg(rgb(0x20252d))
                .border(1.)
                .border_color(rgb(0x66788c))
                .rounded(4.)
                .on_mount(|node| {
                    node.focus();
                }),
        )
}

fn history(slot: Slot, store: SpecStore, mode: Signal<Mode>, offset: Signal<f32>) -> View {
    let current = mode.clone();
    column()
        .grow()
        .min_h(0.)
        .gap(5.)
        .child(
            control("Current revision")
                .id(slot.id("spec-current"))
                .on_click(move || {
                    current.set(Mode::Current);
                }),
        )
        .child(scroll(offset).grow().min_h(0.).child(switch(
            move || store.value.with(|spec| spec.revisions.len()),
            move |count, _| {
                column().gap(4.).children((1..=count).rev().map(|number| {
                    let mode = mode.clone();
                    control(format!("Revision {number}"))
                        .id(slot.id(&format!("spec-revision-{number}")))
                        .on_click(move || {
                            mode.set(Mode::Revision(number));
                        })
                }))
            },
        )))
}

fn document(
    slot: Slot,
    store: SpecStore,
    mode: Signal<Mode>,
    draft: Signal<String>,
    offset: Signal<f32>,
    revision: Option<usize>,
    open_work: impl Fn(&str) + 'static,
    implementation_label: String,
) -> View {
    let history = mode.clone();
    let revision_number = store.value.clone();
    let content = store.value.clone();
    let link = store.value.clone();
    let buttons = row()
        .gap(5.)
        .child(
            control("History")
                .id(slot.id("spec-history"))
                .on_click(move || {
                    history.set(Mode::History);
                }),
        )
        .child(
            text_signal(move || {
                format!(
                    "Revision {}",
                    revision.unwrap_or_else(|| revision_number.with(|spec| spec.revisions.len()))
                )
            })
            .id(slot.id("spec-version"))
            .h(30.)
            .text_color(rgb(0xa3a3a3))
            .text_size(12.),
        );
    let buttons = if revision.is_none() {
        buttons.child(control("Edit").id(slot.id("spec-edit")).on_click(move || {
            let base = store.value.with(|spec| {
                draft.set(spec.revisions.last().unwrap().clone());
                spec.revisions.len()
            });
            mode.set(Mode::Edit(base));
        }))
    } else {
        buttons
    };
    column()
        .grow()
        .min_h(0.)
        .gap(5.)
        .child(buttons)
        .child(
            control(implementation_label)
                .id(slot.id("spec-implementation"))
                .accessible_description("Open linked implementation work")
                .on_click(move || link.with(|spec| open_work(&spec.implementation))),
        )
        .child(
            scroll(offset).grow().min_h(0.).w_full().child(
                text_signal(move || {
                    content.with(|spec| {
                        let index = revision.unwrap_or(spec.revisions.len()) - 1;
                        spec.revisions[index].clone()
                    })
                })
                .id(slot.id("spec-body"))
                .w_full()
                .text_wrap(true)
                .text_size(13.)
                .text_color(rgb(0xd4d4d4)),
            ),
        )
}
