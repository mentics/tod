//! Visual design panel — a mockup `WebView` (left) next to an embedded agent
//! chat (right), opened from the `design` lifecycle state's gate checklist
//! (`design-planning.visual-packages-accepted-or-waived`).
//!
//! Unlike a normal agent chat (which owns its own OS window, see
//! `app::InteractiveAgentWindowControl`), the chat here is constructed
//! in-process via `InteractiveAgentView::with_embedded(true)` so it can sit
//! side by side with the mockup preview in one drawer panel, mirroring the
//! `ObligationsView`/`LifecyclePanelView` drawer convention (see CLAUDE.md's
//! GPUI keyboard-focus section).
//!
//! The mockup preview polls for the node's most recent "Visual design
//! package" obligation (written by `tod-cli visual-design save`) the same
//! way `ObligationsView` polls `fleet.subscribe_changes()` for live updates,
//! and reloads the `WebView` when a new package appears.

use crate::app::InteractiveAgentWindowControl;
use crate::interview::agent::SharedAgent;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use crate::ui::panel_split::{PanelSplitState, h_panel_split};
use crate::views::interactive_agent::InteractiveAgentView;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, Styled, Timer, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::webview::WebView;
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tod_store::TodSettings;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

const VISUAL_DESIGN_PANEL_CONTEXT: &str = "VisualDesignPanel";
const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// Marker in an obligation body left by `tod-cli visual-design save` (see
/// `crates/tod-cli/src/visual_design.rs`).
const PACKAGE_MARKER: &str = "Visual design package:";

actions!(visual_design_panel, [VisualDesignPanelClose]);

#[derive(Debug, Clone)]
pub enum VisualDesignPanelEvent {
    Close,
    /// Ctrl+Left — move keyboard focus back to the task tree, leaving the
    /// panel open (mirrors the drawer-panel convention in CLAUDE.md).
    FocusTaskList,
}

/// Everything needed to construct the embedded chat, assembled by the shell
/// (mirroring `open_obligations_agent_chat`'s call into
/// `InteractiveAgentWindowControl`).
pub struct EmbeddedChatParams {
    pub config_id: String,
    pub session_run_id: String,
    pub fleet: Arc<FleetStore>,
    pub agent: SharedAgent,
    pub workspace_cwd: PathBuf,
    pub settings: TodSettings,
    pub window_control: InteractiveAgentWindowControl,
    /// Assembled app context, sent once ahead of the session's first message.
    pub initial_context: Option<String>,
}

pub fn register_visual_design_panel_keyboard_bindings(cx: &mut App) {
    // Plain arrow keys are needed by both the webview (scrolling) and the
    // chat's text input, so — like `ObligationsView` — only Ctrl+arrows cross
    // back to the task tree here.
    bind_modified_pane_nav(cx, VISUAL_DESIGN_PANEL_CONTEXT);
    key_context::bind_panel_escape(cx, VisualDesignPanelClose, VISUAL_DESIGN_PANEL_CONTEXT);
}

pub struct VisualDesignPanelView {
    fleet: Arc<FleetStore>,
    node_id: Option<Uuid>,
    title: String,
    split: Entity<PanelSplitState>,
    webview: Option<Entity<WebView>>,
    /// A URL waiting to become a `WebView` — deferred because building one
    /// needs a real `Window`, which the poll task that discovers a new
    /// package does not have.
    pending_webview_url: Option<String>,
    chat: Option<Entity<InteractiveAgentView>>,
    /// Id of the obligation whose mockup is currently loaded, so a newer
    /// package can be detected without re-parsing every poll tick.
    loaded_obligation_id: Option<Uuid>,
    focus_handle: FocusHandle,
    _poll_task: gpui::Task<()>,
}

impl VisualDesignPanelView {
    pub fn new(fleet: Arc<FleetStore>, cx: &mut Context<Self>) -> Self {
        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        let _poll_task = cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            loop {
                Timer::after(POLL_INTERVAL).await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if !changed {
                    continue;
                }
                let Ok(()) = poll_entity.update(cx, |this, cx| this.reload_if_new_package(cx))
                else {
                    break;
                };
            }
        });

        Self {
            fleet,
            node_id: None,
            title: String::new(),
            split: cx.new(|_| PanelSplitState::centered()),
            webview: None,
            pending_webview_url: None,
            chat: None,
            loaded_obligation_id: None,
            focus_handle: cx.focus_handle(),
            _poll_task,
        }
    }

    pub fn is_open(&self) -> bool {
        self.node_id.is_some()
    }

    /// Open the panel for `node_id`, constructing the embedded chat fresh —
    /// sessions are never reused, mirroring every other agent-chat entry
    /// point in the app.
    pub fn open(
        &mut self,
        node_id: Uuid,
        title: &str,
        chat: EmbeddedChatParams,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.node_id = Some(node_id);
        self.title = title.to_string();
        self.loaded_obligation_id = None;
        self.webview = None;
        self.pending_webview_url = None;

        let view = cx.new(|cx| {
            InteractiveAgentView::new(
                chat.config_id,
                chat.session_run_id,
                chat.fleet,
                chat.agent,
                chat.workspace_cwd,
                chat.window_control,
                chat.initial_context,
                chat.settings,
                window,
                cx,
            )
            .with_embedded(true)
        });
        self.chat = Some(view);

        self.reload_if_new_package(cx);
        self.focus_handle.focus(window);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.node_id.is_none() {
            return;
        }
        self.node_id = None;
        self.title.clear();
        self.chat = None;
        self.webview = None;
        self.pending_webview_url = None;
        self.loaded_obligation_id = None;
        cx.emit(VisualDesignPanelEvent::Close);
        cx.notify();
    }

    fn on_close_action(
        &mut self,
        _: &VisualDesignPanelClose,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close(cx);
    }

    /// Look up the node's most recent "Visual design package" obligation and,
    /// if it's newer than what's loaded, (re)create the webview pointed at
    /// its file.
    fn reload_if_new_package(&mut self, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_id else { return };
        let Some((obligation_id, path)) = latest_package(&self.fleet, node_id) else {
            return;
        };
        if Some(obligation_id) == self.loaded_obligation_id {
            return;
        }
        self.loaded_obligation_id = Some(obligation_id);
        let url = path_to_file_url(&path);
        match &self.webview {
            Some(webview) => {
                webview.update(cx, |webview, _| webview.load_url(&url));
            }
            None => {
                // A `WebView` needs a real `wry::WebView` bound to the current
                // window; deferred so it's constructed with the render pass's
                // `Window`, not the (window-less) poll task.
                self.pending_webview_url = Some(url);
            }
        }
        cx.notify();
    }
}

impl EventEmitter<VisualDesignPanelEvent> for VisualDesignPanelView {}

impl Focusable for VisualDesignPanelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for VisualDesignPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open() {
            return div().size_full().into_any_element();
        }

        if let Some(url) = self.pending_webview_url.take() {
            let wry_webview = wry::WebViewBuilder::new()
                .with_url(&url)
                .build_as_child(window)
                .ok();
            self.webview = wry_webview.map(|wv| cx.new(|cx| WebView::new(wv, window, cx)));
        }

        let theme = cx.theme();
        let left: gpui::AnyElement = match &self.webview {
            Some(webview) => webview.clone().into_any_element(),
            None => div()
                .size_full()
                .v_flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .text_sm()
                .child("No mockup saved yet — accept a package in the chat to preview it here.")
                .into_any_element(),
        };
        let right: gpui::AnyElement = match &self.chat {
            Some(chat) => chat.clone().into_any_element(),
            None => div().size_full().into_any_element(),
        };

        v_flex()
            .key_context(VISUAL_DESIGN_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(Self::on_close_action))
            .on_action(cx.listener(|_this, _: &PaneFocusLeft, _, cx| {
                cx.emit(VisualDesignPanelEvent::FocusTaskList);
                cx.stop_propagation();
            }))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(format!("Visual design — {}", self.title)),
                    )
                    .child(
                        Button::new("visual-design-panel-close")
                            .label("Close")
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    h_panel_split("visual-design-split", &self.split)
                        .min_left(px(320.))
                        .min_right(px(320.))
                        .left(left)
                        .right(right),
                ),
            )
            .into_any_element()
    }
}

/// Find the file path linked from the most recently created "Visual design
/// package" obligation on `node_id` (see `tod-cli visual-design save`).
fn latest_package(fleet: &FleetStore, node_id: Uuid) -> Option<(Uuid, PathBuf)> {
    let rows = fleet.list_obligations_for_node(node_id).ok()?;
    let row = rows
        .iter()
        .filter(|o| o.body.contains(PACKAGE_MARKER))
        .max_by_key(|o| o.ordinal)?;
    let path = extract_link_path(&row.body)?;
    Some((row.id, path))
}

/// Pull the path out of a trailing markdown link `[title](path)`.
fn extract_link_path(body: &str) -> Option<PathBuf> {
    let start = body.rfind("](")? + 2;
    let end = body[start..].find(')')? + start;
    Some(PathBuf::from(&body[start..end]))
}

fn path_to_file_url(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    if !normalized.starts_with('/') {
        normalized = format!("/{normalized}");
    }
    format!("file://{normalized}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_link_path_reads_trailing_markdown_link() {
        let body = "Visual design package: **Login**\n\n[Login](/data/visual-design/n/login.html)";
        assert_eq!(
            extract_link_path(body),
            Some(PathBuf::from("/data/visual-design/n/login.html"))
        );
    }

    #[test]
    fn path_to_file_url_produces_a_leading_slash() {
        assert_eq!(
            path_to_file_url(Path::new("/home/user/x.html")),
            "file:///home/user/x.html"
        );
        assert_eq!(
            path_to_file_url(Path::new("C:/data/x.html")),
            "file:///C:/data/x.html"
        );
    }
}
