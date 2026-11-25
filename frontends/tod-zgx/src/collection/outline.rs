use super::model::{Chip, Column, Config, Item, Row, RowKey, visible_outline};
use super::{Collection, Panel, SavedCollection, Slot, panel_view};
use std::{cell::Cell, collections::BTreeSet, rc::Rc, sync::Arc};
use tod_palette::{Context, Kind};
use tod_store::outline::types::FlatNodeRow;
use zgx::{compose::prelude::*, reactive::Signal, semantics::Live};
use zgx_desktop::WindowContext;

pub(crate) struct OutlineData {
    name: String,
    rows: Vec<Row>,
    collapsed: BTreeSet<RowKey>,
}

impl OutlineData {
    pub fn new(name: String) -> Self {
        Self {
            name,
            rows: Vec::new(),
            collapsed: BTreeSet::new(),
        }
    }

    pub fn push(&mut self, row: FlatNodeRow, kind: Kind) {
        let id: Arc<str> = row.node.id.to_string().into();
        let key = RowKey::item(id.clone());
        if row.collapsed && row.has_children {
            self.collapsed.insert(key.clone());
        }
        let detail = match row.ticket_id {
            Some(ticket) => format!("Node {} · {ticket}", row.node.slug),
            None => format!("Node {}", row.node.slug),
        };
        self.rows.push(Row {
            key,
            name: row.node.title.clone(),
            item: Some(Item {
                id,
                name: row.node.title.into(),
                kind: kind.label(),
                status: "",
                phase: row.lifecycle.unwrap_or_default().into(),
                tags: row.tags.into(),
                cost: None,
                parent: row.parent_id.map(|id| id.to_string().into()),
                detail: Some(detail.into()),
            }),
            depth: row.depth,
            expandable: row.has_children,
        });
    }
}

#[derive(Clone, PartialEq)]
enum State {
    Empty,
    Loading(String),
    Ready,
    Error(String),
}

#[derive(Clone)]
pub(crate) struct OutlinePanel {
    collection: Collection,
    request: Rc<Cell<u64>>,
    state: Signal<State>,
    rows: Signal<Arc<[Row]>>,
}

impl OutlinePanel {
    pub fn new(cx: &WindowContext) -> Self {
        let panel = cx.ui.signal(Panel::new(None));
        let rows = cx.ui.signal(Arc::from([]));
        let saved = Rc::new(SavedCollection {
            config: cx.ui.signal(Config {
                name: String::new(),
                chips: vec![Chip::Show(vec![Column::Kind, Column::Phase, Column::Tags])],
            }),
            path: None,
            panels: vec![panel.clone()],
            runtime: cx.ui.runtime.clone(),
        });
        Self {
            collection: Collection {
                slot: Slot::Left,
                saved,
                panel,
                draft: cx.ui.signal(String::new()),
                completion: cx.ui.signal(None),
                choice: cx.ui.signal(None),
                offset: cx.ui.signal(0.),
                list_focused: cx.ui.signal(false),
                focus: Default::default(),
                items: Rc::from([]),
                specs: None,
                outline: Some(rows.clone()),
            },
            request: Default::default(),
            state: cx.ui.signal(State::Empty),
            rows,
        }
    }

    pub fn begin(&self, title: String) -> u64 {
        let request = self.request.get().wrapping_add(1);
        self.request.set(request);
        self.state.set(State::Loading(title));
        request
    }

    pub fn finish(&self, request: u64, result: Result<OutlineData, String>) -> bool {
        if self.request.get() != request {
            return false;
        }
        let outline = match result {
            Ok(outline) => outline,
            Err(error) => {
                self.state.set(State::Error(error));
                return false;
            }
        };
        let mut panel = Panel::new(
            visible_outline(&outline.rows, &outline.collapsed)
                .first()
                .map(|row| row.key.clone()),
        );
        panel.collapsed = outline.collapsed;
        self.collection.saved.runtime.batch(|| {
            self.collection
                .saved
                .config
                .update(|config| config.name = outline.name);
            self.rows.set(outline.rows.into());
            self.collection.panel.set(panel);
            self.collection.offset.set(0.);
            self.state.set(State::Ready);
        });
        true
    }

    pub fn ready(&self) -> bool {
        self.state.with(|state| *state == State::Ready)
    }

    pub fn focus(&self) {
        self.collection.focus(|focus| focus.list.as_ref());
    }

    pub fn focused_item(&self) -> Option<String> {
        self.palette_context()?.focused
    }

    pub fn palette_context(&self) -> Option<Context> {
        if !self.ready() {
            return None;
        }
        let item_id = |key: &RowKey| match key {
            RowKey::Item(id) => Some(id.to_string()),
            RowKey::Group(_) => None,
        };
        Some(self.collection.panel.with(|panel| {
            Context {
                focused: panel.cursor.as_ref().and_then(item_id),
                selected: panel.marked.iter().filter_map(item_id).collect(),
                scope: self
                    .collection
                    .saved
                    .config
                    .with(|config| Some(config.name.clone())),
            }
        }))
    }

    pub fn view(&self, cx: &mut WindowContext) -> View {
        let message = self.state.clone();
        let body = self.state.clone();
        column()
            .id("saved-outline")
            .w_full()
            .h_full()
            .gap(8.)
            .child(switch_or_none(
                move || message.get(),
                |state, _| {
                    let message = match state {
                        State::Empty => "Open a saved outline from the command palette.".into(),
                        State::Loading(title) => format!("Opening {title}…"),
                        State::Error(error) => error,
                        State::Ready => return None,
                    };
                    Some(text(message).p(12.).text_wrap(true).live(Live::Polite))
                },
            ))
            .child(
                panel_view(cx, self.collection.clone(), Default::default()).reactive_style(
                    move || {
                        if body.with(|state| *state == State::Ready) {
                            Styles::new()
                        } else {
                            Styles::new().hidden()
                        }
                    },
                ),
            )
    }
}
