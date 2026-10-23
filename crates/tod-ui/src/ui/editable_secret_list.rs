//! An editable list of rows, each a key and a secret, with a trash button per
//! row and an Add button below: a repository and its token, a host and its
//! API key. Reusable by any Settings section that keeps secrets.
//!
//! The inputs are ordinary form inputs: always editable, Tab moves between
//! them, and a click focuses one (the surrounding view's own "focus me on
//! every click" handler is stopped here). Enter in an input does nothing and
//! Escape hands focus back to the view (`ListEvent::Leave`); both are bound
//! one context deeper than a surface's own Enter/Escape and registered after
//! them (`register_keyboard_bindings`), so the surface never captures them
//! while a row is being typed in.
//!
//! The secret is write-only. Its input is masked; a row whose secret is
//! already kept elsewhere (`RowData::stored`) shows a "stored" placeholder,
//! and typing in the field replaces it while leaving it empty keeps it. The
//! list never holds a stored secret, only what the user typed since.
//!
//! The list only edits rows in memory: the owner reads them back with
//! [`EditableSecretList::values`] when the user saves.

use crate::ui::style;
use gpui::{
    actions, App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Render, SharedString, Styled, Window,
    div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable, h_flex, v_flex};

const CONTEXT: &str = "EditableSecretList";

actions!(editable_secret_list, [ListEnter, ListEscape]);

/// Enter and Escape inside a row's inputs. Call after the surface's own
/// bindings are registered: ties in context depth go to the later binding.
pub fn register_keyboard_bindings(cx: &mut App) {
    let in_input: &'static str = Box::leak(format!("{CONTEXT} > Input").into_boxed_str());
    cx.bind_keys([
        KeyBinding::new("enter", ListEnter, Some(in_input)),
        KeyBinding::new("escape", ListEscape, Some(in_input)),
    ]);
}

/// What the list says happened, for the owner's journey record.
pub enum ListEvent {
    Added,
    /// A row was removed; `stored` says it had a stored secret.
    Removed { stored: bool },
    /// Escape: the view should take keyboard focus back.
    Leave,
}

/// The wording the list shows.
#[derive(Clone)]
pub struct Labels {
    pub key_placeholder: SharedString,
    pub secret_placeholder: SharedString,
    pub secret_stored_placeholder: SharedString,
    pub add: SharedString,
    pub remove_tooltip: SharedString,
    /// Shown under a row whose secret is stored.
    pub stored_note: SharedString,
}

/// A row to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowData {
    pub key: String,
    pub stored: bool,
    /// Opaque to the list: the owner's handle on what the row came from.
    pub tag: String,
}

/// A row as the user left it. `secret` is only what was typed (trimmed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowValues {
    pub key: String,
    pub secret: String,
    pub stored: bool,
    pub tag: String,
}

struct Row {
    uid: u64,
    key: Entity<InputState>,
    secret: Entity<InputState>,
    stored: bool,
    tag: String,
}

pub struct EditableSecretList {
    labels: Labels,
    rows: Vec<Row>,
    next_uid: u64,
    focus_handle: FocusHandle,
}

impl EventEmitter<ListEvent> for EditableSecretList {}

impl EditableSecretList {
    pub fn new(labels: Labels, cx: &mut Context<Self>) -> Self {
        Self { labels, rows: Vec::new(), next_uid: 0, focus_handle: cx.focus_handle() }
    }

    /// Replace every row (what is typed is lost).
    pub fn set_rows(&mut self, rows: Vec<RowData>, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.clear();
        for data in rows {
            self.push_row(data, window, cx);
        }
        cx.notify();
    }

    pub fn values(&self, cx: &App) -> Vec<RowValues> {
        self.rows
            .iter()
            .map(|row| {
                let secret: String = row.secret.read(cx).text().to_string();
                RowValues {
                    key: row.key.read(cx).text().to_string().trim().to_string(),
                    secret: secret.trim().chars().filter(|c| !c.is_control()).collect(),
                    stored: row.stored,
                    tag: row.tag.clone(),
                }
            })
            .collect()
    }

    /// Focus the first row's key, or the list when it is empty.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.rows.first() {
            Some(row) => row.key.update(cx, |s, cx| s.focus(window, cx)),
            None => self.focus_handle.focus(window, cx),
        }
    }

    fn push_row(&mut self, data: RowData, window: &mut Window, cx: &mut Context<Self>) {
        let key_placeholder = self.labels.key_placeholder.clone();
        let secret_placeholder = if data.stored {
            self.labels.secret_stored_placeholder.clone()
        } else {
            self.labels.secret_placeholder.clone()
        };
        let key = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(key_placeholder);
            s.set_value(data.key.clone(), window, cx);
            s
        });
        let secret = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(secret_placeholder));
        self.next_uid += 1;
        self.rows.push(Row { uid: self.next_uid, key, secret, stored: data.stored, tag: data.tag });
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.push_row(RowData { key: String::new(), stored: false, tag: String::new() }, window, cx);
        if let Some(row) = self.rows.last() {
            row.key.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.emit(ListEvent::Added);
        cx.notify();
    }

    fn remove(&mut self, uid: u64, cx: &mut Context<Self>) {
        if let Some(i) = self.rows.iter().position(|r| r.uid == uid) {
            let row = self.rows.remove(i);
            cx.emit(ListEvent::Removed { stored: row.stored });
            cx.notify();
        }
    }
}

impl Render for EditableSecretList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = v_flex()
            .id("editable-secret-list")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .gap_2()
            .w_full()
            .on_action(cx.listener(|_, _: &ListEnter, _, _| {}))
            .on_action(cx.listener(|_, _: &ListEscape, _, cx| cx.emit(ListEvent::Leave)))
            // A surface that focuses itself on every click must not take
            // focus from the input just clicked.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for row in &self.rows {
            let uid = row.uid;
            col = col.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().min_w_0().child(Input::new(&row.key)))
                            .child(div().flex_1().min_w_0().child(Input::new(&row.secret)))
                            .child(
                                Button::new(("editable-secret-remove", uid))
                                    .icon(gpui_kit_assets::IconName::Trash)
                                    .ghost()
                                    .compact()
                                    .tooltip(self.labels.remove_tooltip.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove(uid, cx))),
                            ),
                    )
                    .when_stored(row.stored, &self.labels.stored_note),
            );
        }
        col.child(
            h_flex().child(
                Button::new("editable-secret-add")
                    .icon(gpui_component::IconName::Plus)
                    .label(self.labels.add.clone())
                    .compact()
                    .disabled(false)
                    .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
            ),
        )
    }
}

/// `when`-style helper kept local so the render above reads straight through.
trait WhenStored: ParentElement + Sized {
    fn when_stored(self, stored: bool, note: &SharedString) -> Self {
        if stored {
            self.child(style::text_dense_muted(div()).child(note.clone()))
        } else {
            self
        }
    }
}
impl<T: ParentElement + Sized> WhenStored for T {}
