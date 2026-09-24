//! `ColumnPanel`: the contract every panel shown in a unified-view column
//! implements, and `PlaceholderPanel`, the stand-in used for every panel kind
//! until later work items (W5, W7, W9) replace them with real ones.

use std::sync::Arc;

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Render, SharedString, Styled, Window, actions, div,
    prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, Sizable};
use tod_store::fleet::FleetStore;

use super::columns::PanelKind;

actions!(
    unified_panel,
    [PanelActivateFocusedLink, PanelCtrlActivateFocusedLink]
);

pub const UNIFIED_PANEL_CONTEXT: &str = "UnifiedPanel";

/// What any panel hosted in a unified-view column must offer the root: a
/// title for its column header, and a target description for placeholders
/// and tests.
pub trait ColumnPanel {
    /// The column header's title for this panel.
    fn title(&self, cx: &App) -> SharedString;

    /// What this panel is showing, for the column header's subtitle.
    fn target_label(&self, cx: &App) -> SharedString;
}

/// Emitted when the user activates a link inside a panel — a click, ctrl
/// click, or Enter/Ctrl+Enter on the focused link. The root applies the
/// column-placement rule (`ColumnModel::open`) using `from_column` (this
/// panel's own column index, supplied by the root when it renders).
#[derive(Debug, Clone)]
pub struct PanelOpenRequest {
    pub target: PanelKind,
    pub ctrl: bool,
}

/// One link inside a placeholder panel: a label and the panel it opens.
#[derive(Debug, Clone)]
struct PlaceholderLink {
    label: SharedString,
    target: PanelKind,
}

/// Renders a panel's title and target, with a couple of links to other
/// panels so the column-placement rule can be exercised end to end before
/// W5/W7/W9 land the real panels.
pub struct PlaceholderPanel {
    kind: PanelKind,
    fleet: Arc<FleetStore>,
    focus_handle: FocusHandle,
    links: Vec<PlaceholderLink>,
    selected_link: usize,
}

impl PlaceholderPanel {
    pub fn new(kind: PanelKind, fleet: Arc<FleetStore>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            kind,
            fleet,
            focus_handle: cx.focus_handle(),
            links: Vec::new(),
            selected_link: 0,
        };
        panel.rebuild_links();
        panel
    }

    pub fn kind(&self) -> PanelKind {
        self.kind
    }

    /// Retarget this column to a different panel kind, in place — used when
    /// the column model replaces or retargets the column this panel backs.
    pub fn set_kind(&mut self, kind: PanelKind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.selected_link = 0;
        self.rebuild_links();
        cx.notify();
    }

    fn rebuild_links(&mut self) {
        let node = match self.kind {
            PanelKind::Details(id)
            | PanelKind::Obligations(id)
            | PanelKind::Plan(id)
            | PanelKind::Settings(id)
            | PanelKind::Transcript(id) => Some(id),
            PanelKind::Decisions => None,
        };
        let mut links = Vec::new();
        if let Some(id) = node {
            if !matches!(self.kind, PanelKind::Details(_)) {
                links.push(PlaceholderLink {
                    label: "Details".into(),
                    target: PanelKind::Details(id),
                });
            }
            if !matches!(self.kind, PanelKind::Obligations(_)) {
                links.push(PlaceholderLink {
                    label: "Obligations".into(),
                    target: PanelKind::Obligations(id),
                });
            }
            if !matches!(self.kind, PanelKind::Plan(_)) {
                links.push(PlaceholderLink {
                    label: "Plan".into(),
                    target: PanelKind::Plan(id),
                });
            }
            if !matches!(self.kind, PanelKind::Settings(_)) {
                links.push(PlaceholderLink {
                    label: "Settings".into(),
                    target: PanelKind::Settings(id),
                });
            }
            links.push(PlaceholderLink {
                label: "Decisions".into(),
                target: PanelKind::Decisions,
            });
        }
        self.links = links;
    }

    fn activate(&self, ctrl: bool, cx: &mut Context<Self>) {
        if let Some(link) = self.links.get(self.selected_link) {
            cx.emit(PanelOpenRequest {
                target: link.target,
                ctrl,
            });
        }
    }

    fn target_text(&self, cx: &App) -> String {
        match self.kind {
            PanelKind::Details(id)
            | PanelKind::Obligations(id)
            | PanelKind::Plan(id)
            | PanelKind::Settings(id)
            | PanelKind::Transcript(id) => self
                .fleet
                .get_task(&id.to_string())
                .ok()
                .flatten()
                .map(|t| t.title)
                .unwrap_or_else(|| id.to_string()),
            PanelKind::Decisions => {
                let _ = cx;
                "no node current".into()
            }
        }
    }
}

impl ColumnPanel for PlaceholderPanel {
    fn title(&self, _cx: &App) -> SharedString {
        match self.kind {
            PanelKind::Details(_) => "Details".into(),
            PanelKind::Decisions => "Decisions".into(),
            PanelKind::Obligations(_) => "Obligations".into(),
            PanelKind::Plan(_) => "Plan".into(),
            PanelKind::Settings(_) => "Settings".into(),
            PanelKind::Transcript(_) => "Transcript".into(),
        }
    }

    fn target_label(&self, cx: &App) -> SharedString {
        self.target_text(cx).into()
    }
}

impl EventEmitter<PanelOpenRequest> for PlaceholderPanel {}

impl Focusable for PlaceholderPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let target = self.target_text(cx);
        div()
            .id("unified-placeholder-panel")
            .key_context(UNIFIED_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &PanelActivateFocusedLink, _, cx| {
                this.activate(false, cx);
            }))
            .on_action(cx.listener(|this, _: &PanelCtrlActivateFocusedLink, _, cx| {
                this.activate(true, cx);
            }))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .size_full()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("Target: {target}")),
            )
            .children(self.links.iter().enumerate().map(|(ix, link)| {
                let selected = ix == self.selected_link;
                let target = link.target;
                div()
                    .id(("unified-panel-link", ix))
                    .when(selected, |el| {
                        el.border_1().border_color(theme.accent)
                    })
                    .child(
                        Button::new(("unified-panel-link-btn", ix))
                            .label(link.label.clone())
                            .small()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.selected_link = ix;
                                    let ctrl = event.modifiers.control || event.modifiers.platform;
                                    cx.emit(PanelOpenRequest { target, ctrl });
                                    cx.notify();
                                }),
                            ),
                    )
            }))
    }
}
