mod completion;
mod model;
mod specification;

use completion::{Choice, Completion, Draft, Edit, Stage};
use model::{Chip, Column, Config, Item, Row, RowKey};
use specification::{SpecStore, Specification};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    io,
    path::PathBuf,
    rc::Rc,
};
use zgx::{
    compose::{NodeHandle, ViewHandle, prelude::*},
    input::{EventContext, EventPhase, InputEvent, Key, Modifiers, PointerButton},
    reactive::{Runtime, Signal},
    semantics::Live,
};
use zgx_desktop::WindowContext;

const ROW_HEIGHT: f32 = 30.;
const PAGE_ROWS: usize = 8;
const LIST_HEIGHT: f32 = (PAGE_ROWS as f32 + 1.) * ROW_HEIGHT;
const MUTED: u32 = 0xa3a3a3;
const CONTROL: u32 = 0x232323;
const CURSOR: u32 = 0x23426a;
pub(super) const ID: &str = "design-work";

#[derive(Clone, PartialEq)]
struct Panel {
    open: bool,
    chip: usize,
    edit: Option<ChipEdit>,
    cursor: Option<RowKey>,
    range: Option<(usize, BTreeSet<RowKey>)>,
    marked: BTreeSet<RowKey>,
    collapsed: BTreeSet<RowKey>,
    inspected: Option<Item>,
    error: String,
}

#[derive(Clone, PartialEq)]
struct ChipEdit {
    index: Option<usize>, // None adds a chip.
    base: Config,
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Left,
    Right,
}

impl Slot {
    const ALL: [Self; 2] = [Self::Left, Self::Right];

    fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }

    fn id(self, name: &str) -> String {
        format!("{}-{name}", self.label())
    }
}

const CONFIG_CHANGED: &str =
    "The saved config changed in the other panel. Press Escape to review it.";

struct SavedCollection {
    config: Signal<Config>,
    path: PathBuf,
    panels: [Signal<Panel>; 2],
    runtime: Runtime,
}

#[derive(Default)]
struct Focus {
    header: Option<NodeHandle>,
    strip: Option<NodeHandle>,
    list: Option<NodeHandle>,
}

#[derive(Clone)]
struct Collection {
    slot: Slot,
    saved: Rc<SavedCollection>,
    panel: Signal<Panel>,
    draft: Signal<String>,
    completion: Signal<Option<Completion>>,
    choice: Signal<Option<usize>>,
    offset: Signal<f32>,
    list_focused: Signal<bool>,
    focus: Rc<RefCell<Focus>>,
    items: Rc<[Item]>,
    specs: SpecStore,
}

impl Collection {
    fn rows(&self) -> Vec<Row> {
        self.saved.config.with(|config| {
            self.panel
                .with(|panel| model::rows(&self.items, config, &panel.collapsed))
        })
    }

    fn focus(&self, target: impl FnOnce(&Focus) -> Option<&NodeHandle>) {
        let node = target(&self.focus.borrow()).cloned();
        if let Some(node) = node
            && !node.focus()
        {
            let later = node.clone();
            node.after_layout(move || {
                later.focus();
            });
        }
    }

    fn toggle_config(&self) {
        let open = self.panel.with(|panel| panel.open);
        let config = self.saved.config.get();
        self.clear_completion();
        self.panel.update(|panel| {
            panel.open = !open;
            if let Some(edit) = panel.edit.take() {
                panel.chip = remap_chip(panel.chip, &edit.base, &config);
            }
            panel.error.clear();
        });
        if open {
            self.focus(|focus| focus.header.as_ref());
        }
    }

    fn start_edit(&self, index: Option<usize>) {
        let base = self.saved.config.get();
        self.draft
            .set(index.map(|i| base.chips[i].label()).unwrap_or_default());
        self.panel.update(|panel| {
            panel.edit = Some(ChipEdit { index, base });
            panel.error.clear();
        });
    }

    fn cancel_edit(&self) {
        self.clear_completion();
        let config = self.saved.config.get();
        self.panel.update(|panel| {
            if let Some(edit) = panel.edit.take() {
                panel.chip = remap_chip(panel.chip, &edit.base, &config);
            }
            panel.error.clear();
        });
        self.focus(|focus| focus.strip.as_ref());
    }

    fn clear_completion(&self) {
        self.completion.set(None);
        self.choice.set(None);
    }

    fn commit_chip(&self, chip: Chip) {
        let Some(edit) = self.panel.with(|panel| panel.edit.clone()) else {
            return;
        };
        let mut config = self.saved.config.get();
        if edit.base != config {
            return;
        }
        let next = config.apply(edit.index, chip);
        if self.save(config, next) {
            self.clear_completion();
            self.focus(|focus| focus.strip.as_ref());
        }
    }

    fn save(&self, config: Config, chip: usize) -> bool {
        if let Err(error) = config.save(&self.saved.path) {
            self.panel
                .update(|panel| panel.error = format!("Could not save config. {error}"));
            return false;
        }
        let previous = self.saved.config.get();
        let visible: BTreeSet<_> = model::rows(&self.items, &config, &BTreeSet::new())
            .into_iter()
            .map(|row| row.key)
            .collect();
        self.saved.runtime.batch(|| {
            for (slot, panel) in Slot::ALL.into_iter().zip(&self.saved.panels) {
                panel.update(|panel| {
                    if slot == self.slot {
                        panel.chip = chip;
                        panel.edit = None;
                        panel.error.clear();
                    } else if panel.edit.is_none() {
                        panel.chip = remap_chip(panel.chip, &previous, &config);
                    }
                    panel.marked.retain(|key| visible.contains(key));
                    let old_rows = model::rows(&self.items, &previous, &panel.collapsed);
                    let rows = model::rows(&self.items, &config, &panel.collapsed);
                    if old_rows != rows {
                        panel.range = None;
                    }
                    if !rows
                        .iter()
                        .any(|row| Some(&row.key) == panel.cursor.as_ref())
                    {
                        let prior_index = old_rows
                            .iter()
                            .position(|row| Some(&row.key) == panel.cursor.as_ref())
                            .unwrap_or(0);
                        panel.cursor = rows
                            .get(prior_index.min(rows.len().saturating_sub(1)))
                            .map(|row| row.key.clone());
                    }
                });
            }
            self.saved.config.set(config);
        });
        true
    }

    fn chip_key(&self, key: &Key) -> bool {
        let (cursor, editing) = self.panel.with(|panel| (panel.chip, panel.edit.is_some()));
        if editing {
            return false;
        }
        let len = self.saved.config.with(|config| config.chips.len());
        match key {
            Key::ArrowLeft | Key::ArrowUp => {
                self.panel
                    .update(|panel| panel.chip = cursor.saturating_sub(1));
            }
            Key::ArrowRight | Key::ArrowDown => {
                self.panel
                    .update(|panel| panel.chip = (cursor + 1).min(len.saturating_sub(1)));
            }
            Key::Home => {
                self.panel.update(|panel| panel.chip = 0);
            }
            Key::End => {
                self.panel
                    .update(|panel| panel.chip = len.saturating_sub(1));
            }
            Key::Space => self.start_edit(None),
            Key::Enter => self.start_edit((len > 0).then_some(cursor)),
            Key::Delete | Key::Backspace if len > 0 => {
                let mut config = self.saved.config.get();
                config.chips.remove(cursor);
                self.save(config, cursor.min(len.saturating_sub(2)));
            }
            Key::Escape => self.toggle_config(),
            _ => return false,
        }
        true
    }

    fn move_row(&self, rows: &[Row], from: usize, to: usize, extend: bool) {
        self.panel.update(|panel| {
            if extend {
                let (anchor, baseline) = panel
                    .range
                    .get_or_insert_with(|| (from, panel.marked.clone()));
                panel.marked.clone_from(baseline);
                for row in &rows[(*anchor).min(to)..=(*anchor).max(to)] {
                    if row.item.is_some() {
                        panel.marked.insert(row.key.clone());
                    }
                }
            } else {
                panel.range = None;
            }
            panel.cursor = Some(rows[to].key.clone());
        });
        self.reveal_row(to);
    }

    fn reveal_row(&self, index: usize) {
        let position = index as f32 * ROW_HEIGHT;
        let offset = self.offset.get();
        if position < offset {
            self.offset.set(position);
        } else if position + ROW_HEIGHT > offset + LIST_HEIGHT {
            self.offset.set(position + ROW_HEIGHT - LIST_HEIGHT);
        }
    }

    fn list_focus_changed(&self, focused: bool) {
        self.list_focused.set(focused);
        if focused {
            let cursor = self.panel.with(|panel| panel.cursor.clone());
            if let Some(index) = self
                .rows()
                .iter()
                .position(|row| Some(&row.key) == cursor.as_ref())
            {
                self.reveal_row(index);
            }
        }
    }

    fn toggle_branch(&self, row: &Row) {
        self.panel.update(|panel| {
            if !panel.collapsed.remove(&row.key) {
                panel.collapsed.insert(row.key.clone());
            }
            panel.range = None;
            panel.cursor = Some(row.key.clone());
        });
    }

    fn activate(&self, row: &Row) {
        if let Some(item) = row.item {
            self.panel.update(|panel| {
                panel.cursor = Some(row.key.clone());
                panel.range = None;
                panel.inspected = Some(item);
            });
        } else {
            self.toggle_branch(row);
        }
    }

    fn list_key(&self, key: &Key, shift: bool) -> bool {
        if matches!(key, Key::Character(value) if value == "/") {
            if !self.panel.with(|panel| panel.open) {
                self.toggle_config();
            }
            self.focus(|focus| focus.strip.as_ref());
            return true;
        }
        let rows = self.rows();
        if rows.is_empty() {
            return false;
        }
        let index = self.panel.with(|panel| {
            rows.iter()
                .position(|row| Some(&row.key) == panel.cursor.as_ref())
                .unwrap_or(0)
        });
        let row = &rows[index];
        match key {
            Key::ArrowDown | Key::ArrowUp => {
                let delta = if *key == Key::ArrowDown { 1 } else { -1 };
                let mut next = index as isize + delta;
                while shift
                    && rows
                        .get(next as usize)
                        .is_some_and(|row| row.item.is_none())
                {
                    next += delta;
                }
                if rows.get(next as usize).is_some() {
                    self.move_row(&rows, index, next as usize, shift);
                }
            }
            Key::PageUp => self.move_row(&rows, index, index.saturating_sub(PAGE_ROWS), false),
            Key::PageDown => {
                self.move_row(&rows, index, (index + PAGE_ROWS).min(rows.len() - 1), false)
            }
            Key::Home => self.move_row(&rows, index, 0, false),
            Key::End => self.move_row(&rows, index, rows.len() - 1, false),
            Key::Space if row.item.is_some() => {
                self.panel.update(|panel| {
                    panel.range = None;
                    if !panel.marked.remove(&row.key) {
                        panel.marked.insert(row.key.clone());
                    }
                });
            }
            Key::ArrowLeft => {
                if row.expandable && !self.panel.with(|panel| panel.collapsed.contains(&row.key)) {
                    self.toggle_branch(row);
                } else if row.item.is_some()
                    && let Some(parent) = rows[..index].iter().rposition(|candidate| {
                        candidate.depth < row.depth || candidate.item.is_none()
                    })
                {
                    // Rows are in display order. A group heading is the parent of its top-level rows.
                    self.move_row(&rows, index, parent, false);
                }
            }
            Key::ArrowRight => {
                if row.expandable && self.panel.with(|panel| panel.collapsed.contains(&row.key)) {
                    self.toggle_branch(row);
                }
            }
            Key::Enter => self.activate(row),
            _ => return false,
        }
        true
    }
}

fn remap_chip(index: usize, previous: &Config, config: &Config) -> usize {
    previous
        .chips
        .get(index)
        .and_then(|selected| config.chips.iter().position(|chip| chip == selected))
        .unwrap_or_else(|| index.min(config.chips.len().saturating_sub(1)))
}

fn key_event(event: &mut EventContext, handler: impl FnOnce(&Key, Modifiers) -> bool) {
    if event.phase == EventPhase::Target
        && let InputEvent::KeyDown { key, modifiers, .. } = &event.event
        && handler(key, *modifiers)
    {
        event.prevent_default();
    }
}

fn control(label: impl Into<String>) -> View {
    button()
        .h(30.)
        .px(9.)
        .rounded(5.)
        .bg(rgb(CONTROL))
        .hover(|style| style.bg(rgb(0x373737)))
        .child(text(label))
}

#[derive(Clone)]
struct InlineEditor {
    collection: Collection,
    node: Rc<RefCell<Option<NodeHandle>>>,
    focused: Rc<Cell<bool>>,
    popup: Signal<bool>,
    existing: bool,
}

impl InlineEditor {
    fn refresh(&self) -> Option<Completion> {
        let selection = self.node.borrow().as_ref()?.selection()?;
        let draft = self.collection.draft.get();
        let options =
            Draft::parse(&draft).complete(selection, self.existing, &self.collection.items);
        if self.collection.completion.set(Some(options.clone())) {
            self.collection.choice.set(None);
        }
        self.popup.set(!options.choices.is_empty());
        Some(options)
    }

    fn hide(&self) {
        self.popup.set(false);
        self.collection.clear_completion();
    }

    fn choose(&self, choice: &Choice, stage: Stage) {
        let raw = self.collection.draft.get();
        let Some((_, caret)) = self.node.borrow().as_ref().and_then(NodeHandle::selection) else {
            return;
        };
        match Draft::parse(&raw).choose(choice, stage, caret) {
            Some(edit) => self.apply_edit(edit),
            None => {
                self.collection.panel.update(|panel| {
                    panel.error = "Choose an operator for this field.".into();
                });
            }
        }
    }

    fn apply_edit(&self, edit: Edit) {
        match edit {
            Edit::Commit(chip) => self.collection.commit_chip(chip),
            Edit::Continue { text, caret } => {
                let Some(editor) = self.node.borrow().as_ref().and_then(NodeHandle::editor) else {
                    return;
                };
                editor.replace_text(0..self.collection.draft.with(String::len), &text);
                editor.set_selection(caret, caret);
                self.collection.choice.set(None);
                self.collection.panel.update(|panel| panel.error.clear());
                self.refresh();
            }
        }
    }

    fn column_comma(&self) -> bool {
        let raw = self.collection.draft.get();
        let draft = Draft::parse(&raw);
        if !draft.is_show() {
            return false;
        }
        let Some((_, caret)) = self.node.borrow().as_ref().and_then(NodeHandle::selection) else {
            return false;
        };
        let highlighted = self.refresh().and_then(|options| {
            match options.choices.get(self.collection.choice.get()?)? {
                Choice::Column(column) => Some(*column),
                _ => None,
            }
        });
        let Some(edit) = draft.column_comma(caret, highlighted) else {
            return false;
        };
        self.apply_edit(edit);
        true
    }

    fn key(&self, key: &Key, modifiers: Modifiers, repeat: bool) -> bool {
        if modifiers.control || modifiers.alt || modifiers.meta {
            return false;
        }
        if !matches!(key, Key::ArrowUp | Key::ArrowDown | Key::Space | Key::Enter) {
            return false;
        }
        let Some(options) = self.refresh() else {
            return false;
        };
        let raw = self.collection.draft.get();
        match key {
            Key::ArrowUp | Key::ArrowDown if !options.choices.is_empty() && !modifiers.shift => {
                let last = options.choices.len() - 1;
                let next = match (self.collection.choice.get(), key) {
                    (Some(index), Key::ArrowUp) => index.saturating_sub(1),
                    (Some(index), _) => (index + 1).min(last),
                    (None, Key::ArrowUp) => last,
                    (None, _) => 0,
                };
                self.collection.choice.set(Some(next));
            }
            Key::Space
                if matches!(options.stage, Stage::Field | Stage::Operator)
                    && !options.term.is_empty()
                    && self.node.borrow().as_ref().and_then(NodeHandle::selection)
                        == Some((raw.len(), raw.len())) =>
            {
                let Some(choice) = options.preferred() else {
                    return false;
                };
                if !repeat {
                    self.choose(choice, options.stage);
                }
            }
            Key::Enter => {
                if repeat {
                    return true;
                }
                let draft = Draft::parse(&raw);
                if let Some(choice) = self
                    .collection
                    .choice
                    .get()
                    .and_then(|i| options.choices.get(i))
                {
                    self.choose(choice, options.stage);
                } else if let Some(result) = draft.show_columns() {
                    match result {
                        Ok(chip) => self.collection.commit_chip(chip),
                        Err(issue) => {
                            if let Some(node) = self.node.borrow().as_ref() {
                                node.set_selection(issue.selection.0, issue.selection.1);
                            }
                            self.collection
                                .panel
                                .update(|panel| panel.error = issue.message);
                            self.refresh();
                        }
                    }
                } else if let Some(chip) = draft.chip() {
                    self.collection.commit_chip(chip);
                } else {
                    let options = draft.complete(
                        (raw.len(), raw.len()),
                        self.existing,
                        &self.collection.items,
                    );
                    if let Some(choice) = options.preferred() {
                        self.choose(choice, options.stage);
                    } else {
                        self.collection.panel.update(|panel| {
                            panel.error = "Finish the chip or choose a suggestion.".into();
                        });
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn event(&self, event: &mut EventContext) {
        if event.phase != EventPhase::Target {
            return;
        }
        match &event.event {
            InputEvent::Blur => {
                self.focused.set(false);
                self.hide();
                return;
            }
            InputEvent::Focus => self.focused.set(true),
            InputEvent::KeyDown {
                key,
                modifiers,
                repeat,
            } if self.key(key, *modifiers, *repeat) => {
                event.prevent_default();
                return;
            }
            InputEvent::Text(value) if value == "," && self.column_comma() => {
                event.prevent_default();
                return;
            }
            InputEvent::Text(_) | InputEvent::ImeCommit(_) => {
                self.collection.choice.set(None);
                self.collection.panel.update(|panel| panel.error.clear());
            }
            InputEvent::KeyDown { .. }
            | InputEvent::KeyUp { .. }
            | InputEvent::PointerDown { .. }
            | InputEvent::PointerUp { .. }
            | InputEvent::PointerMove { .. } => {}
            _ => return,
        }
        // ZGX calls view listeners before its editor applies the input or selection.
        if let Some(node) = self.node.borrow().as_ref() {
            let editor = self.clone();
            node.after_layout(move || {
                if editor.focused.get()
                    && editor.collection.panel.with(|panel| panel.edit.is_some())
                {
                    editor.refresh();
                }
            });
        }
    }
}

fn editor_view(collection: &Collection, value_start: usize) -> View {
    let collection = collection.clone();
    component(move |cx| {
        let editor = InlineEditor {
            collection: collection.clone(),
            node: Default::default(),
            focused: Default::default(),
            popup: cx.state(false),
            existing: value_start > 0,
        };
        let mount = editor.clone();
        let events = editor.clone();
        let active = collection.choice.clone();
        let popup_choice = collection.choice.clone();
        let options = collection.completion.clone();
        let selector = collection.choice.clone();
        let slot = collection.slot;
        let input = text_input("Edit collection chip", collection.draft.clone())
            .id(slot.id("chip-editor"))
            .w(280.)
            .h(32.)
            .px(8.)
            .border(1.)
            .border_color(rgb(0x73aaf1))
            .rounded(5.)
            .bg(rgb(0x182433))
            .active_descendant(move || {
                active
                    .get()
                    .map(|index| slot.id(&format!("suggestion-{index}")))
            })
            .on_mount(move |node| {
                mount.node.replace(Some(node.clone()));
                mount.focused.set(node.focus());
                node.set_selection(value_start, mount.collection.draft.get().len());
                mount.refresh();
            })
            .on_event(move |event| events.event(event));
        provide(
            zgx::compose::PopupsInWindow(cx.state(true)),
            popover("Chip suggestions", editor.popup.clone(), input)
                .at_caret()
                .w(195.)
                .p(4.)
                .gap(2.)
                .bg(rgb(0x1b2029))
                .border(1.)
                .border_color(rgb(0x515965))
                .rounded(6.)
                .child(switch_or_none(
                    move || {
                        let options = options.get()?;
                        let start = selector.get().unwrap_or(0).saturating_sub(3);
                        Some((options.stage, options.choices, start))
                    },
                    move |state, _| {
                        let (stage, choices, start) = state?;
                        Some(
                            column()
                                .gap(2.)
                                .child(
                                    text(stage.label())
                                        .px(6.)
                                        .text_size(11.)
                                        .text_color(rgb(MUTED)),
                                )
                                .children(choices.into_iter().enumerate().skip(start).take(4).map(
                                    |(index, choice)| {
                                        let editor = editor.clone();
                                        let selected = popup_choice.clone();
                                        control(choice.label())
                                            .id(slot.id(&format!("suggestion-{index}")))
                                            .w_full()
                                            .h(26.)
                                            .focusable(false)
                                            .focus_on_press(false)
                                            .reactive_style(move || {
                                                Styles::new().bg(rgb(
                                                    if selected.get() == Some(index) {
                                                        CURSOR
                                                    } else {
                                                        CONTROL
                                                    },
                                                ))
                                            })
                                            .on_click(move || editor.choose(&choice, stage))
                                    },
                                )),
                        )
                    },
                )),
        )
    })
}

fn config_view(collection: Collection) -> View {
    let keys = collection.clone();
    let strip_focus = collection.focus.clone();
    let select_strip = collection.panel.clone();
    let strip = collection.clone();
    let control_state = collection.panel.clone();
    let action = collection.clone();
    let notice = collection.panel.clone();
    let error = collection.panel.clone();
    let config = collection.saved.config.clone();
    let saved = config.clone();
    let draft = collection.draft.clone();
    let slot = collection.slot;
    column()
        .w_full()
        .p(8.)
        .gap(6.)
        .bg(rgb(0x141414))
        .rounded(6.)
        .child(switch_or_none(
            move || {
                error.with(|panel| {
                    if panel
                        .edit
                        .as_ref()
                        .is_some_and(|edit| config.with(|latest| &edit.base != latest))
                    {
                        Some(CONFIG_CHANGED.to_owned())
                    } else {
                        (!panel.error.is_empty()).then(|| panel.error.clone())
                    }
                })
            },
            move |message, _| {
                message.map(|message| {
                    text(message)
                        .id(slot.id("config-error"))
                        .text_size(12.)
                        .text_color(rgb(0xffb4a5))
                        .text_wrap(true)
                        .w_full()
                        .live(Live::Polite)
                })
            },
        ))
        .child(
            row()
                .w_full()
                .gap(6.)
                .child(
                    row()
                        .id(slot.id("chip-strip"))
                        .grow()
                        .min_w(0.)
                        .flex_wrap()
                        .gap(5.)
                        .focusable(true)
                        .on_mount(move |node| {
                            node.focus();
                            strip_focus.borrow_mut().strip = Some(node);
                        })
                        .on_event(move |event| key_event(event, |key, _| keys.chip_key(key)))
                        .child(switch(
                            move || {
                                select_strip.with(|panel| match &panel.edit {
                                    Some(edit) => (edit.base.chips.clone(), Some(edit.index)),
                                    None => (saved.with(|config| config.chips.clone()), None),
                                })
                            },
                            move |(chips, edit), _| {
                                let mut line = row().flex_wrap().gap(5.);
                                for (index, chip) in chips.into_iter().enumerate() {
                                    if edit == Some(Some(index)) {
                                        line = line.child(editor_view(&strip, chip.value_start()));
                                    } else {
                                        let selection = strip.panel.clone();
                                        let select = strip.clone();
                                        line = line.child(
                                            control(chip.label())
                                                .focusable(false)
                                                .focus_on_press(false)
                                                .reactive_style(move || {
                                                    Styles::new().bg(rgb(
                                                        if selection
                                                            .with(|panel| panel.chip == index)
                                                        {
                                                            CURSOR
                                                        } else {
                                                            CONTROL
                                                        },
                                                    ))
                                                })
                                                .on_click(move || {
                                                    select.panel.update(|panel| panel.chip = index);
                                                    if select
                                                        .panel
                                                        .with(|panel| panel.edit.is_some())
                                                    {
                                                        select.cancel_edit();
                                                    } else {
                                                        select.focus(|focus| focus.strip.as_ref());
                                                    }
                                                }),
                                        );
                                    }
                                }
                                if edit == Some(None) {
                                    line = line.child(editor_view(&strip, 0));
                                }
                                line
                            },
                        )),
                )
                .child(switch(
                    move || control_state.with(|panel| panel.edit.is_some()),
                    move |active, _| {
                        let action = action.clone();
                        control(if active { "⊘" } else { "+" })
                            .accessible_label(if active { "Cancel edit" } else { "Add chip" })
                            .focusable(!active)
                            .focus_on_press(false)
                            .on_click(move || {
                                if active {
                                    action.cancel_edit();
                                } else {
                                    action.start_edit(None);
                                }
                            })
                    },
                )),
        )
        .child(
            text_signal(move || {
                notice.with(|panel| {
                    if panel.edit.is_some() {
                        if draft.with(|value| Draft::parse(value).is_show()) {
                            "↑ ↓ Suggestions  ·  Comma Next field  ·  Enter Apply  ·  Esc Cancel"
                                .into()
                        } else {
                            "↑ ↓ Suggestions  ·  Enter Apply  ·  Esc Cancel".into()
                        }
                    } else {
                        "← → Browse  ·  Enter Edit  ·  Space Add  ·  Delete Remove".into()
                    }
                })
            })
            .text_size(12.)
            .text_color(rgb(MUTED))
            .h(18.),
        )
}

fn row_node_id(slot: Slot, key: &RowKey) -> String {
    let key = match key {
        RowKey::Group(name) => format!("row-group-{name}"),
        RowKey::Item(id) => format!("row-item-{id}"),
    };
    slot.id(&key)
}

fn column_cell(column: Column, label: impl Into<String>) -> View {
    let width = match column {
        Column::Kind => 116.,
        Column::Status => 65.,
        Column::Phase => 100.,
        Column::Cost => 65.,
        Column::Tags => 95.,
    };
    text(label)
        .w(width)
        .min_w(width)
        .text_size(12.)
        .text_color(rgb(MUTED))
        .text_overflow(TextOverflow::Ellipsis)
}

fn column_headers(slot: Slot, columns: Vec<Column>) -> View {
    row()
        .w_full()
        .h(22.)
        .pl(8.)
        .gap(5.)
        .child(div().w(24.))
        .child(
            text("Title")
                .grow()
                .min_w(0.)
                .px(4.)
                .text_size(12.)
                .text_color(rgb(MUTED)),
        )
        .children(columns.into_iter().map(|column| {
            column_cell(column, column.label()).id(slot.id(&format!("column-{}", column.id())))
        }))
}

fn list_row(collection: &Collection, item: Row, columns: &[Column]) -> View {
    let state = collection.panel.clone();
    let key = item.key.clone();
    let id = row_node_id(collection.slot, &key);
    let focused = collection.list_focused.clone();
    let disclosure = if item.expandable {
        let action = collection.clone();
        let branch = item.clone();
        let name = item.name.clone();
        let key = item.key.clone();
        let state = collection.panel.clone();
        button()
            .w(24.)
            .h(ROW_HEIGHT)
            .focusable(false)
            .focus_on_press(false)
            .accessible_label(format!("Toggle {name}"))
            .child(text_signal(move || {
                if state.with(|panel| panel.collapsed.contains(&key)) {
                    "›".into()
                } else {
                    "⌄".into()
                }
            }))
            .on_click(move || {
                action.toggle_branch(&branch);
                action.focus(|focus| focus.list.as_ref());
            })
    } else {
        div().w(24.)
    };
    let action = collection.clone();
    let activate = item.clone();
    row()
        .w_full()
        .h(ROW_HEIGHT)
        .pl(8. + item.depth.min(5) as f32 * 14.)
        .gap(5.)
        .reactive_style(move || {
            Styles::new().bg(rgb(state.with(|panel| {
                if focused.get() && panel.cursor.as_ref() == Some(&key) {
                    CURSOR
                } else if panel.marked.contains(&key) {
                    0x192e48
                } else {
                    0x191919
                }
            })))
        })
        .child(disclosure)
        .child(
            button()
                .id(id)
                .grow()
                .min_w(0.)
                .h(ROW_HEIGHT)
                .px(4.)
                .focusable(false)
                .focus_on_press(false)
                .bg(rgba(0))
                .hover(|style| style.bg(rgb(0x353535)))
                .child(text(item.name).text_overflow(TextOverflow::Ellipsis))
                .on_click(move || {
                    action.activate(&activate);
                    action.focus(|focus| focus.list.as_ref());
                }),
        )
        .children(columns.iter().copied().map(|column| {
            column_cell(
                column,
                item.item
                    .map_or_else(String::new, |item| column.value(item)),
            )
        }))
}

fn window_keys(
    collections: &[Collection; 2],
    active: Option<Slot>,
    help: &Signal<bool>,
    escape_down: &Cell<bool>,
    event: &mut EventContext,
) {
    if event.phase == EventPhase::Bubble {
        return;
    }
    if matches!(
        event.event,
        InputEvent::KeyDown {
            key: Key::Function(1),
            repeat: false,
            ..
        }
    ) {
        help.update(|shown| *shown = !*shown);
        event.prevent_default();
        return;
    }
    let (pressed, key, modifiers, repeat) = match &event.event {
        InputEvent::KeyDown {
            key,
            modifiers,
            repeat,
        } => (true, key, modifiers, *repeat),
        InputEvent::KeyUp { key, modifiers } => (false, key, modifiers, false),
        _ => return,
    };
    if *key != Key::Escape
        && event.key_code() != Some("Escape")
        && !matches!(key, Key::Character(value) if modifiers.control && value == "[")
    {
        return;
    }

    // The draft belongs to the config even if Tab has already left the editor.
    // Consume the matching release so one press never also closes the strip.
    let handled = if pressed {
        escape_down.get()
    } else {
        escape_down.replace(false)
    };
    let editing = collections.iter().find(|collection| {
        active.is_none_or(|slot| collection.slot == slot)
            && collection.panel.with(|panel| panel.edit.is_some())
    });
    if editing.is_some() || handled {
        if pressed {
            escape_down.set(true);
        }
        if let Some(collection) = editing
            && !repeat
            && (pressed || !handled)
        {
            collection.cancel_edit();
        }
        event.prevent_default();
        event.stop_propagation();
    }
}

fn panel_view(
    cx: &mut WindowContext,
    collection: Collection,
    active: Rc<Cell<Option<Slot>>>,
) -> View {
    let slot = collection.slot;
    let config = collection.saved.config.clone();
    let headers = collection.panel.clone();
    let toggle = collection.clone();
    let header_focus = collection.focus.clone();
    let show_config = collection.panel.clone();
    let config_builder = collection.clone();
    let listen = collection.clone();
    let list_focus = collection.focus.clone();
    let read_rows = collection.clone();
    let build_rows = collection.clone();
    let count = collection.clone();
    let details = collection.panel.clone();
    let requirement = collection.clone();
    let detail_scroll = cx.ui.signal(0.);
    let cursor = collection.panel.clone();
    let display = collection.saved.config.clone();
    column()
        .grow()
        .flex_basis(0.)
        .min_w(0.)
        .h_full()
        .p(10.)
        .gap(7.)
        .bg(rgb(0x111111))
        .border(1.)
        .border_color(rgb(0x333333))
        .rounded(7.)
        .on_event(move |event| {
            if event.phase != EventPhase::Bubble && matches!(event.event, InputEvent::Focus) {
                active.set(Some(slot));
            }
        })
        .child(
            row()
                .gap(8.)
                .child(
                    button()
                        .id(slot.id("config-name"))
                        .h(32.)
                        .px(9.)
                        .rounded(5.)
                        .bg(rgb(CONTROL))
                        .accessible_description(format!(
                            "Configure {} collection panel",
                            slot.label().to_lowercase()
                        ))
                        .child(text_signal(move || {
                            headers.with(|panel| {
                                config.with(|config| {
                                    format!(
                                        "{}  {}",
                                        config.name,
                                        if panel.open { "⌃" } else { "⌄" }
                                    )
                                })
                            })
                        }))
                        .on_mount(move |node| {
                            header_focus.borrow_mut().header = Some(node);
                        })
                        .on_click(move || toggle.toggle_config()),
                )
                .child(div().grow())
                .child(text(slot.label()).text_size(12.).text_color(rgb(MUTED))),
        )
        .child(switch_or_none(
            move || show_config.with(|panel| panel.open),
            move |open, _| open.then(|| config_view(config_builder.clone())),
        ))
        .child(
            text_signal(move || {
                count.panel.with(|panel| {
                    let total = count.saved.config.with(|config| {
                        model::rows(&count.items, config, &BTreeSet::new())
                            .iter()
                            .filter(|row| row.item.is_some())
                            .count()
                    });
                    format!("{total} items  ·  {} marked", panel.marked.len())
                })
            })
            .text_color(rgb(MUTED))
            .text_size(12.)
            .h(18.),
        )
        .child(switch(
            move || display.with(|config| config.columns().to_vec()),
            move |columns, _| column_headers(slot, columns),
        ))
        .child(
            scroll(collection.offset.clone())
                .id(slot.id("collection-pane"))
                .h(LIST_HEIGHT)
                .w_full()
                .focusable(true)
                .accessible_label(format!("{} collection rows", slot.label()))
                .active_descendant(move || {
                    cursor.with(|panel| panel.cursor.as_ref().map(|key| row_node_id(slot, key)))
                })
                .focus(|style| {
                    style
                        .border(1.)
                        .border_over(true)
                        .border_color(rgb(0x73aaf1))
                })
                .on_mount(move |node| {
                    list_focus.borrow_mut().list = Some(node);
                })
                .on_event(move |event| {
                    if event.phase == EventPhase::Target {
                        match event.event {
                            InputEvent::Focus => listen.list_focus_changed(true),
                            InputEvent::Blur => listen.list_focus_changed(false),
                            _ => {}
                        }
                    }
                    key_event(event, |key, modifiers| {
                        listen.list_key(key, modifiers.shift)
                    });
                })
                .child(switch(
                    move || {
                        (
                            read_rows.rows(),
                            read_rows
                                .saved
                                .config
                                .with(|config| config.columns().to_vec()),
                        )
                    },
                    move |(rows, columns), _| {
                        if rows.is_empty() {
                            return text("No matches").p(12.);
                        }
                        column().w_full().children(
                            rows.into_iter()
                                .map(|row| list_row(&build_rows, row, &columns)),
                        )
                    },
                )),
        )
        .child(switch_or_none(
            move || details.with(|panel| panel.inspected),
            move |item, _| {
                let item = item?;
                if item.id == specification::REQUIREMENT {
                    let open_task = requirement.clone();
                    let implementation_label = requirement
                        .items
                        .iter()
                        .find(|work| work.id == specification::IMPLEMENTATION)
                        .expect("implementation work fixture")
                        .name;
                    return Some(specification::inspect(
                        slot,
                        item,
                        requirement.specs.clone(),
                        move |id| {
                            if let Some(item) = open_task.items.iter().find(|item| item.id == id) {
                                open_task
                                    .panel
                                    .update(|panel| panel.inspected = Some(*item));
                            }
                        },
                        implementation_label,
                    ));
                }
                detail_scroll.set(0.);
                Some(
                    column()
                        .grow()
                        .min_h(0.)
                        .gap(5.)
                        .p(9.)
                        .bg(rgb(0x171717))
                        .rounded(6.)
                        .child(
                            text(format!("{}  ·  {}", item.id, item.name))
                                .id(slot.id("inspector-title"))
                                .font_bold()
                                .h(22.),
                        )
                        .child(
                            scroll(detail_scroll.clone())
                                .w_full()
                                .grow()
                                .min_h(0.)
                                .child(
                                    text(item.spec.unwrap_or(
                                        "Mock item in the native collection experiment.",
                                    ))
                                    .w_full()
                                    .text_wrap(true)
                                    .text_size(13.)
                                    .text_color(rgb(0xd4d4d4)),
                                ),
                        ),
                )
            },
        ))
}

pub(super) struct Data {
    config: Config,
    path: PathBuf,
    items: Rc<[Item]>,
    spec: Specification,
    initial_body: &'static str,
}

impl Data {
    pub fn load(path: Option<PathBuf>, smoke: bool) -> io::Result<Self> {
        let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/agent/scratchpad/zgx-collection/config.json");
        let path = if smoke {
            default.with_file_name(format!("smoke-{}.json", std::process::id()))
        } else {
            path.unwrap_or(default)
        };
        let config = if smoke {
            Config::default()
        } else {
            Config::load(&path)?
        };
        let items: Rc<[Item]> = model::items().into();
        let initial_body = items
            .iter()
            .find(|item| item.id == specification::REQUIREMENT)
            .and_then(|item| item.spec)
            .expect("collection requirement fixture");
        let spec = Specification::load(&Specification::path(&path), initial_body)?;
        Ok(Self {
            config,
            path,
            items,
            spec,
            initial_body,
        })
    }
}

#[derive(Clone)]
pub(super) struct Panels {
    collections: [Collection; 2],
    active: Rc<Cell<Option<Slot>>>,
    help: Signal<bool>,
}

impl Panels {
    pub fn new(cx: &mut WindowContext, data: &Data) -> Self {
        let specs = SpecStore::new(
            &cx.ui,
            data.spec.clone(),
            Specification::path(&data.path),
            data.initial_body,
        );
        let cursor = model::rows(&data.items, &data.config, &BTreeSet::new())
            .first()
            .map(|row| row.key.clone());
        let panels = Slot::ALL.map(|_| {
            cx.ui.signal(Panel {
                cursor: cursor.clone(),
                open: false,
                chip: 0,
                edit: None,
                range: None,
                marked: BTreeSet::new(),
                collapsed: BTreeSet::new(),
                inspected: None,
                error: String::new(),
            })
        });
        let saved = Rc::new(SavedCollection {
            config: cx.ui.signal(data.config.clone()),
            path: data.path.clone(),
            panels,
            runtime: cx.ui.runtime.clone(),
        });
        let collections = Slot::ALL.map(|slot| Collection {
            slot,
            panel: saved.panels[slot as usize].clone(),
            saved: saved.clone(),
            draft: cx.ui.signal(String::new()),
            completion: cx.ui.signal(None),
            choice: cx.ui.signal(None),
            offset: cx.ui.signal(0.),
            list_focused: cx.ui.signal(false),
            focus: Default::default(),
            items: data.items.clone(),
            specs: specs.clone(),
        });
        Self {
            collections,
            active: Default::default(),
            help: cx.ui.signal(false),
        }
    }

    pub fn name(&self) -> String {
        self.collections[0]
            .saved
            .config
            .with(|config| config.name.clone())
    }

    pub fn search_items(&self) -> impl Iterator<Item = tod_palette::Item> + '_ {
        use tod_palette::{Item, Kind};
        std::iter::once(Item {
            id: ID.into(),
            title: self.name(),
            kind: Kind::Collection,
            scope: None,
        })
        .chain(
            self.collections[0]
                .items
                .iter()
                .filter(|item| item.kind != "Task")
                .map(|item| Item {
                    id: item.id.into(),
                    title: item.name.into(),
                    kind: Kind::ProductDefinition,
                    scope: Some(self.name()),
                }),
        )
    }

    pub fn contains(&self, id: &str) -> bool {
        id == ID
            || self.collections[0]
                .items
                .iter()
                .any(|item| item.id == id && item.kind != "Task")
    }

    fn active(&self) -> &Collection {
        &self.collections[self.active.get().unwrap_or(Slot::Left) as usize]
    }

    pub fn focus(&self) {
        self.active().focus(|focus| focus.list.as_ref());
    }

    pub fn open_item(&self, id: &str) {
        let collection = self.active();
        if let Some(item) = collection.items.iter().find(|item| item.id == id).copied() {
            let rows = collection.rows();
            if let Some(index) = rows.iter().position(|row| row.key == RowKey::Item(item.id)) {
                collection.move_row(&rows, index, index, false);
            }
            collection
                .panel
                .update(|panel| panel.inspected = Some(item));
        }
        self.focus();
    }

    pub fn open_config(&self) {
        let collection = self.active();
        if !collection.panel.with(|panel| panel.open) {
            collection.toggle_config();
        }
        collection.focus(|focus| focus.strip.as_ref());
    }

    pub fn view(&self, cx: &mut WindowContext) -> View {
        let active = self.active.clone();
        let panels = self
            .collections
            .clone()
            .map(|collection| panel_view(cx, collection, active.clone()));
        let help_button = self.help.clone();
        let help_key = self.help.clone();
        let show_help = self.help.clone();
        let shortcuts = self.collections.clone();
        let escape_down = Cell::new(false);
        column().w_full().h_full().gap(8.)
        .id("collection-panels")
        .on_event(move |event| {
            if event.phase != EventPhase::Bubble && matches!(event.event, InputEvent::Focus) {
                active.set(None);
            }
            window_keys(&shortcuts, active.get(), &help_key, &escape_down, event);
        })
        .child(row().child(div().grow()).child(
            control("Keyboard  F1").on_click(move || { help_button.update(|shown| *shown = !*shown); })
        ))
        .child(switch_or_none(move || show_help.get(), |show, _| show.then(||
            text("↑ ↓ Browse  ·  ← Parent or collapse  ·  → Expand  ·  Shift+↑ ↓ Mark range  ·  Page Up/Down Page  ·  Enter Inspect  ·  / Config")
                .text_size(12.).text_wrap(true).p(7.).bg(rgb(CONTROL))
        )))
        .child(row().grow().min_h(0.).w_full().gap(12.).children(panels))
    }

    pub fn smoke_test(&self, ui: &mut zgx::widgets::Ui, view: &ViewHandle) {
        smoke_test(ui, &self.collections, &self.help, view);
        std::fs::remove_file(&self.collections[0].saved.path).expect("remove smoke config");
    }
}

fn smoke_test(
    ui: &mut zgx::widgets::Ui,
    collections: &[Collection; 2],
    help: &Signal<bool>,
    view: &ViewHandle,
) {
    let [collection, other] = collections;
    let find = |id| view.find(&Slot::Left.id(id));
    fn press(ui: &mut zgx::widgets::Ui, key: Key, shift: bool) {
        let modifiers = Modifiers {
            shift,
            ..Default::default()
        };
        ui.dispatch(InputEvent::KeyDown {
            key: key.clone(),
            modifiers,
            repeat: false,
        });
        ui.dispatch(InputEvent::KeyUp { key, modifiers });
    }

    fn click(ui: &mut zgx::widgets::Ui, view: &ViewHandle, id: &str) {
        ui.prepare_frame();
        let node = view.find(id).unwrap_or_else(|| panic!("missing {id}"));
        let bounds = ui
            .scene
            .borrow()
            .visible_bounds(node)
            .unwrap_or_else(|| panic!("{id} is hidden"));
        let (x, y) = (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.);
        let button = PointerButton::Primary;
        ui.dispatch(InputEvent::PointerDown { x, y, button });
        ui.dispatch(InputEvent::PointerUp { x, y, button });
    }

    fn labels(collection: &Collection, stage: Stage) -> Vec<String> {
        let completion = collection
            .completion
            .get()
            .expect("editor completion state");
        assert_eq!(completion.stage, stage);
        completion
            .choices
            .iter()
            .map(|choice| choice.label().to_owned())
            .collect()
    }

    fn navigate_to(ui: &mut zgx::widgets::Ui, collection: &Collection, key: &RowKey) {
        press(ui, Key::Home, false);
        let target = collection
            .rows()
            .iter()
            .position(|row| &row.key == key)
            .unwrap_or_else(|| panic!("missing {key:?}"));
        for _ in 0..target {
            press(ui, Key::ArrowDown, false);
        }
    }

    let list = collection.focus.borrow().list.as_ref().unwrap().node();
    press(ui, Key::Tab, true);
    assert_ne!(ui.input.focused(), Some(list));
    press(ui, Key::Tab, false);
    assert_eq!(ui.input.focused(), Some(list));

    let cursor = || collection.panel.with(|panel| panel.cursor.clone());
    let visible = |key: &RowKey| collection.rows().iter().any(|row| &row.key == key);
    let group = RowKey::Group("Product component".into());
    let component = RowKey::Item("collection-panel");
    press(ui, Key::ArrowDown, false);
    assert_eq!(cursor().as_ref(), Some(&component));
    press(ui, Key::ArrowLeft, false);
    assert_eq!(cursor().as_ref(), Some(&group));
    assert!(visible(&component));
    press(ui, Key::ArrowLeft, false);
    assert_eq!(cursor().as_ref(), Some(&group));
    assert!(!visible(&component));
    press(ui, Key::ArrowRight, false);
    assert_eq!(cursor().as_ref(), Some(&group));
    assert!(visible(&component));

    press(ui, Key::ArrowDown, false);
    press(ui, Key::ArrowDown, true);
    press(ui, Key::ArrowDown, true);
    assert_eq!(collection.panel.with(|panel| panel.marked.len()), 3);
    press(ui, Key::ArrowUp, true);
    assert_eq!(collection.panel.with(|panel| panel.marked.len()), 2);
    press(ui, Key::PageDown, false);
    assert!(collection.offset.get() > 0.);

    press(ui, Key::Character("/".into()), false);
    let strip = collection.focus.borrow().strip.as_ref().unwrap().node();
    assert_eq!(ui.input.focused(), Some(strip));
    press(ui, Key::Enter, false);
    assert_eq!(ui.focused_editor().unwrap().selection(), (7, 13));
    assert_eq!(
        labels(collection, Stage::Value),
        ["Design", "Specification"]
    );
    press(ui, Key::ArrowDown, false);
    press(ui, Key::ArrowDown, false);
    press(ui, Key::Enter, false);
    assert_eq!(ui.input.focused(), Some(strip));
    assert_eq!(
        collection
            .saved
            .config
            .with(|config| config.chips[0].label()),
        "Tag is Specification"
    );

    press(ui, Key::Enter, false);
    assert_eq!(ui.focused_editor().unwrap().selection(), (7, 20));
    assert_eq!(
        labels(collection, Stage::Value),
        ["Design", "Specification"]
    );
    press(ui, Key::ArrowRight, false);
    assert_eq!(labels(collection, Stage::Value), ["Specification"]);
    for _ in 0..14 {
        press(ui, Key::ArrowLeft, false);
    }
    assert_eq!(ui.focused_editor().unwrap().selection(), (6, 6));
    assert_eq!(labels(collection, Stage::Operator), ["is", "is not"]);
    click(ui, view, &Slot::Left.id("suggestion-1"));
    assert!(ui.focused_editor().is_some());
    assert_eq!(collection.draft.get(), "Tag is not Specification");
    assert!(collection.panel.with(|panel| panel.edit.is_some()));
    for _ in 0..10 {
        press(ui, Key::ArrowLeft, false);
    }
    assert_eq!(labels(collection, Stage::Field), ["Tag", "Text"]);
    press(ui, Key::ArrowLeft, false);
    assert!(labels(collection, Stage::Field).is_empty());

    press(ui, Key::Tab, false);
    assert_eq!(ui.input.focused(), Some(list));
    assert!(collection.completion.get().is_none());
    press(ui, Key::Escape, false);
    assert_eq!(ui.input.focused(), Some(strip));
    assert_eq!(
        collection
            .saved
            .config
            .with(|config| config.chips[0].label()),
        "Tag is Specification"
    );

    press(ui, Key::Space, false);
    assert!(ui.focused_editor().is_some());
    ui.dispatch(InputEvent::Text("order".into()));
    ui.prepare_frame();
    assert_eq!(labels(collection, Stage::Field), ["Sort by"]);
    press(ui, Key::ArrowDown, false);
    assert_eq!(collection.choice.get(), Some(0));
    press(ui, Key::Function(1), false);
    assert!(help.get());
    press(ui, Key::Escape, false);
    assert_eq!(ui.input.focused(), Some(strip));
    assert!(collection.panel.with(|panel| panel.edit.is_none()));
    press(ui, Key::ArrowRight, false);
    assert_eq!(collection.panel.with(|panel| panel.chip), 1);

    press(ui, Key::Space, false);
    ui.dispatch(InputEvent::Text("nest".into()));
    press(ui, Key::Space, false);
    assert_eq!(collection.draft.get(), "Nest by ");
    assert_eq!(labels(collection, Stage::Value), ["Part of"]);
    press(ui, Key::Enter, false);
    assert_eq!(
        collection
            .saved
            .config
            .with(|config| config.chips.last().cloned()),
        Some(Chip::Nest)
    );
    assert!(collection.panel.with(|panel| panel.edit.is_none()));

    collection.focus(|focus| focus.list.as_ref());
    navigate_to(ui, collection, &RowKey::Item("REQ-COLLECTION-01"));
    press(ui, Key::Enter, false);
    assert_eq!(
        collection
            .panel
            .with(|panel| panel.inspected.map(|item| item.id)),
        Some("REQ-COLLECTION-01")
    );

    press(ui, Key::Character("/".into()), false);
    press(ui, Key::Home, false);
    press(ui, Key::Enter, false);
    press(ui, Key::ArrowDown, false);
    press(ui, Key::Enter, false);
    press(ui, Key::Tab, false); // Add chip control
    press(ui, Key::Tab, false);
    assert_eq!(ui.input.focused(), Some(list));
    let nested_child = RowKey::Item("check-focus");
    let nested_parent = RowKey::Item("verify-keyboard");
    navigate_to(ui, collection, &nested_child);
    press(ui, Key::Space, false);
    press(ui, Key::ArrowLeft, false);
    assert_eq!(cursor().as_ref(), Some(&nested_parent));
    assert!(visible(&nested_child));
    press(ui, Key::ArrowLeft, false);
    assert_eq!(cursor().as_ref(), Some(&nested_parent));
    assert!(!visible(&nested_child));
    assert!(
        collection
            .panel
            .with(|panel| panel.marked.contains(&nested_child))
    );
    press(ui, Key::ArrowRight, false);
    assert_eq!(cursor().as_ref(), Some(&nested_parent));
    assert!(visible(&nested_child));

    other.focus(|focus| focus.list.as_ref());
    press(ui, Key::Home, false);
    press(ui, Key::ArrowLeft, false);
    press(ui, Key::End, false);
    press(ui, Key::Space, false);
    let other_offset = other.offset.get();
    assert!(other_offset > 0.);
    let other_rows = other.rows();
    let other_cursor = other.panel.with(|panel| panel.cursor.clone());
    let other_collapsed = other.panel.with(|panel| panel.collapsed.clone());
    let other_marks = other.panel.with(|panel| panel.marked.clone());
    assert!(!other_collapsed.is_empty());
    assert!(!other_marks.is_empty());
    collection.focus(|focus| focus.list.as_ref());

    let rows_before_show = collection.rows();
    let marks_before_show = collection.panel.with(|panel| panel.marked.clone());
    press(ui, Key::Character("/".into()), false);
    press(ui, Key::Space, false);
    ui.dispatch(InputEvent::Text("columns".into()));
    press(ui, Key::Space, false);
    assert_eq!(collection.draft.get(), "Show ");
    ui.dispatch(InputEvent::Text("co".into()));
    ui.dispatch(InputEvent::Text(",".into()));
    assert_eq!(collection.draft.get(), "Show Agent cost, ");
    ui.dispatch(InputEvent::Text("t".into()));
    ui.dispatch(InputEvent::Text(",".into()));
    assert_eq!(collection.draft.get(), "Show Agent cost, t, ");
    ui.dispatch(InputEvent::Text("ph".into()));
    press(ui, Key::Enter, false);
    assert!(find("config-error").is_some());
    assert!(
        collection
            .panel
            .with(|panel| panel.error.contains("matches multiple fields"))
    );
    assert_eq!(labels(collection, Stage::Column), ["Item type", "Tags"]);
    let position = "Show Agent cost, t".len();
    assert_eq!(
        ui.focused_editor().unwrap().selection(),
        (position, position)
    );
    press(ui, Key::ArrowDown, false);
    press(ui, Key::Enter, false);
    assert_eq!(collection.draft.get(), "Show Agent cost, Item type, ph");
    press(ui, Key::Enter, false);
    assert!(find("column-status").is_none());
    assert!(view.find(&Slot::Right.id("column-status")).is_none());
    assert!(view.find(&Slot::Right.id("column-agent-cost")).is_some());
    assert_eq!(other.offset.get(), other_offset);
    assert_eq!(other.rows(), other_rows);
    other.panel.with(|panel| {
        assert_eq!(panel.cursor, other_cursor);
        assert_eq!(panel.collapsed, other_collapsed);
        assert_eq!(panel.marked, other_marks);
    });
    assert_eq!(collection.rows(), rows_before_show);
    assert_eq!(cursor().as_ref(), Some(&nested_parent));
    assert_eq!(
        collection.panel.with(|panel| panel.marked.clone()),
        marks_before_show
    );

    let column_x = |column: Column| {
        let id = format!("column-{}", column.id());
        let node = view
            .find(&Slot::Left.id(&id))
            .expect("visible column header");
        ui.scene.borrow().transformed_bounds(node).x
    };
    assert!(column_x(Column::Cost) < column_x(Column::Kind));
    assert!(column_x(Column::Kind) < column_x(Column::Phase));

    press(ui, Key::Enter, false);
    ui.dispatch(InputEvent::Text(
        "Item type, Lifecycle phase, Agent cost".into(),
    ));
    press(ui, Key::Enter, false);
    ui.prepare_frame();
    let cost = find("column-agent-cost").unwrap();
    let phase = find("column-lifecycle-phase").unwrap();
    let scene = ui.scene.borrow();
    assert!(scene.transformed_bounds(phase).x < scene.transformed_bounds(cost).x);
    drop(scene);
    assert_eq!(
        Config::load(&collection.saved.path).unwrap().columns(),
        [Column::Kind, Column::Phase, Column::Cost]
    );

    press(ui, Key::Enter, false);
    ui.dispatch(InputEvent::Text("zzz, Tags".into()));
    press(ui, Key::Enter, false);
    assert!(
        collection
            .panel
            .with(|panel| panel.error.contains("matches no available field"))
    );
    assert_eq!(ui.focused_editor().unwrap().selection(), (5, 8));
    assert!(labels(collection, Stage::Column).contains(&"Agent cost".into()));
    press(ui, Key::Escape, false);
    assert!(find("config-error").is_none());
    assert!(find("column-tags").is_none());
    assert!(find("column-agent-cost").is_some());

    other.focus(|focus| focus.list.as_ref());
    press(ui, Key::Character("/".into()), false);
    press(ui, Key::Home, false);
    press(ui, Key::ArrowRight, false);
    press(ui, Key::Enter, false);
    ui.dispatch(InputEvent::Text("Tags".into()));
    collection.focus(|focus| focus.strip.as_ref());
    press(ui, Key::Enter, false);
    ui.dispatch(InputEvent::Text("Agent cost, Status".into()));
    press(ui, Key::Enter, false);
    assert_eq!(other.draft.get(), "Show Tags");
    assert!(view.find(&Slot::Right.id("config-error")).is_some());
    let right_editor = view.find(&Slot::Right.id("chip-editor")).unwrap();
    assert!(ui.input.focus(&ui.scene, Some(right_editor)));
    press(ui, Key::Enter, false);
    assert_eq!(other.draft.get(), "Show Tags");
    assert_eq!(
        other.saved.config.with(|config| config.columns().to_vec()),
        [Column::Cost, Column::Status]
    );
    press(ui, Key::Escape, false);
    assert!(view.find(&Slot::Right.id("config-error")).is_none());
    press(ui, Key::Delete, false);
    for slot in Slot::ALL {
        assert!(view.find(&slot.id("column-status")).is_some());
        assert!(view.find(&slot.id("column-agent-cost")).is_none());
    }
}
