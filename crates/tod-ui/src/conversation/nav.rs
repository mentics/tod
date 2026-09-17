//! The header's drill-down: the nodes under the focused item, as a tree the
//! user can expand and pick a new focus from.
//!
//! The tree is loaded once when the menu opens — the outline does not change
//! under it — and flattened for display against the expanded set.

use super::{ConversationView, Pane};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use anyhow::Result;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::{Icon, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use tod_store::conversation::Focus;
use tod_store::outline::repos::{ListRepo, NodeRepo, OutlineRepo};
use uuid::Uuid;

/// One row of the drill-down: a node, `depth` levels below the menu's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavRow {
    pub node: Uuid,
    pub title: String,
    pub depth: usize,
    pub has_children: bool,
}

/// The subtree under the focused item, as the menu shows it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct NavTree {
    /// The focus's own children, in outline order.
    roots: Vec<Uuid>,
    children: HashMap<Uuid, Vec<Uuid>>,
    titles: HashMap<Uuid, String>,
}

impl NavTree {
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The rows to show, top to bottom, given what is expanded.
    pub fn rows(&self, expanded: &HashSet<Uuid>) -> Vec<NavRow> {
        let mut rows = Vec::new();
        self.walk(&self.roots, 0, expanded, &mut rows);
        rows
    }

    fn walk(&self, ids: &[Uuid], depth: usize, expanded: &HashSet<Uuid>, out: &mut Vec<NavRow>) {
        for id in ids {
            let children = self.children.get(id).map_or(&[][..], Vec::as_slice);
            out.push(NavRow {
                node: *id,
                title: self.titles.get(id).cloned().unwrap_or_default(),
                depth,
                has_children: !children.is_empty(),
            });
            if expanded.contains(id) {
                self.walk(children, depth + 1, expanded, out);
            }
        }
    }
}

/// The open drill-down.
#[derive(Debug)]
pub(crate) struct NavMenu {
    tree: NavTree,
    expanded: HashSet<Uuid>,
    /// The highlighted row of `tree.rows(&expanded)`.
    highlight: usize,
}

/// Whether `root` has anything under it to drill into — children, or any
/// top-level node when the focus is the project. Read with the rest of the
/// snapshot, so the header does not hit the store to draw a frame.
pub(super) fn has_children(conn: &Connection, root: Option<Uuid>) -> Result<bool> {
    let outline = OutlineRepo::new(conn);
    let lists = match root {
        Some(id) => match outline.get_entry(id)? {
            Some(entry) => vec![entry.list_id],
            None => Vec::new(),
        },
        None => ListRepo::new(conn)
            .list_all()?
            .into_iter()
            .map(|list| list.id)
            .collect(),
    };
    for list in lists {
        if outline
            .list_for_list(list)?
            .iter()
            .any(|entry| entry.parent_id == root)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

impl ConversationView {
    /// The nodes under `root`, or the top-level nodes of every list when the
    /// focus is the project.
    pub(super) fn load_nav_tree(&self, root: Option<Uuid>) -> NavTree {
        self.fleet
            .read(|conn| {
                let outline = OutlineRepo::new(conn);
                let nodes = NodeRepo::new(conn);
                // Per list, so top-level nodes keep their list's grouping
                // instead of interleaving by ordinal.
                let mut lists = Vec::new();
                match root {
                    Some(id) => {
                        if let Some(entry) = outline.get_entry(id)? {
                            lists.push(outline.list_for_list(entry.list_id)?);
                        }
                    }
                    None => {
                        for list in ListRepo::new(conn).list_all()? {
                            lists.push(outline.list_for_list(list.id)?);
                        }
                    }
                }
                let mut tree = NavTree::default();
                for mut entries in lists {
                    entries.sort_by_key(|e| e.ordinal);
                    for entry in entries {
                        let Some(node) = nodes.get(entry.node_id)? else {
                            continue;
                        };
                        tree.titles.insert(node.id, node.title);
                        match entry.parent_id {
                            Some(parent) => tree.children.entry(parent).or_default().push(node.id),
                            None if root.is_none() => tree.roots.push(node.id),
                            None => {}
                        }
                    }
                }
                if let Some(id) = root {
                    tree.roots = tree.children.get(&id).cloned().unwrap_or_default();
                }
                Ok(tree)
            })
            .unwrap_or_default()
    }

    /// Open the drill-down on the focused item's children, if it has any.
    pub(super) fn open_nav_menu(&mut self, cx: &mut Context<Self>) {
        let tree = self.load_nav_tree(self.data.focus_node);
        if tree.is_empty() {
            return;
        }
        self.picker = None;
        self.pane = Pane::Transcript;
        self.nav = Some(NavMenu {
            tree,
            expanded: HashSet::new(),
            highlight: 0,
        });
        cx.notify();
    }

    pub(super) fn close_nav_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.nav.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    pub(super) fn nav_rows(&self) -> Vec<NavRow> {
        self.nav
            .as_ref()
            .map(|nav| nav.tree.rows(&nav.expanded))
            .unwrap_or_default()
    }

    /// Move the drill-down's highlight; false when it is not open.
    pub(super) fn nav_move(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let rows = self.nav_rows();
        let Some(nav) = self.nav.as_mut() else {
            return false;
        };
        if !rows.is_empty() {
            let last = rows.len() as isize - 1;
            nav.highlight = (nav.highlight as isize + delta).clamp(0, last) as usize;
            cx.notify();
        }
        true
    }

    /// Expand the highlighted row, or step into it when it is already open;
    /// false when the drill-down is not open.
    pub(super) fn nav_expand(&mut self, cx: &mut Context<Self>) -> bool {
        let rows = self.nav_rows();
        let Some(nav) = self.nav.as_mut() else {
            return false;
        };
        if let Some(row) = rows.get(nav.highlight).filter(|r| r.has_children) {
            if !nav.expanded.insert(row.node) {
                nav.highlight += 1;
            }
            cx.notify();
        }
        true
    }

    /// Collapse the highlighted row, or move up to its parent; false when
    /// the drill-down is not open.
    pub(super) fn nav_collapse(&mut self, cx: &mut Context<Self>) -> bool {
        let rows = self.nav_rows();
        let Some(nav) = self.nav.as_mut() else {
            return false;
        };
        let Some(row) = rows.get(nav.highlight) else {
            return true;
        };
        if nav.expanded.remove(&row.node) {
            cx.notify();
            return true;
        }
        if let Some(parent) = rows[..nav.highlight]
            .iter()
            .rposition(|r| r.depth + 1 == row.depth)
        {
            nav.highlight = parent;
            cx.notify();
        }
        true
    }

    /// Focus on the highlighted node; false when the drill-down is not open.
    pub(super) fn nav_activate(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let rows = self.nav_rows();
        let Some(nav) = self.nav.as_ref() else {
            return false;
        };
        let node = rows.get(nav.highlight).map(|row| row.node);
        if let Some(node) = node {
            self.choose_nav_node(node, window, cx);
        }
        true
    }

    /// Talk about `node` instead, remembering where we were.
    pub(super) fn choose_nav_node(
        &mut self,
        node: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.nav = None;
        self.open(Focus::Node(node), true, window, cx);
    }

    fn toggle_nav_expanded(&mut self, node: Uuid, cx: &mut Context<Self>) {
        if let Some(nav) = self.nav.as_mut() {
            if !nav.expanded.remove(&node) {
                nav.expanded.insert(node);
            }
            cx.notify();
        }
    }

    pub(super) fn render_nav_menu(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = self.nav_rows();
        let highlight = self.nav.as_ref().map_or(0, |nav| nav.highlight);
        let expanded = self
            .nav
            .as_ref()
            .map(|nav| nav.expanded.clone())
            .unwrap_or_default();
        let mut menu = style::floating_panel(v_flex())
            .id("conversation-nav-menu")
            .min_w(px(280.))
            .max_w(px(480.))
            .max_h(px(420.))
            .overflow_y_scroll()
            .gap(style::space::HAIRLINE)
            .px(style::space::INLINE)
            .py(style::space::INLINE)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_nav_menu(cx);
            }));
        for (ix, row) in rows.iter().enumerate() {
            let node = row.node;
            let open = expanded.contains(&node);
            let has_children = row.has_children;
            menu = menu.child(
                style::menu_item(h_flex(), ix == highlight)
                    .id(ElementId::Name(format!("nav-entry-{ix}").into()))
                    .w_full()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.choose_nav_node(node, window, cx);
                        }),
                    )
                    .child(div().flex_shrink_0().w(px(row.depth as f32 * 12.)))
                    .child(
                        div()
                            .id(ElementId::Name(format!("nav-twisty-{ix}").into()))
                            .flex_shrink_0()
                            .w(px(16.))
                            .when(has_children, |el| {
                                el.cursor_pointer()
                                    .child(
                                        Icon::new(if open {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .xsmall(),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_nav_expanded(node, cx);
                                        }),
                                    )
                            }),
                    )
                    .child(
                        selectable_text(
                            ElementId::Name(format!("nav-title-{ix}").into()),
                            row.title.clone(),
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis(),
                    ),
            );
        }
        menu.into_any_element()
    }
}
