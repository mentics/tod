//! Visual design panel — a mockup `WebView` (left) next to an embedded agent
//! chat (right), opened from the "Design"/"+ Design" affordance on a
//! design-phase obligation's row in the Obligations panel. Each panel
//! session is scoped to exactly one obligation — the mockup shown and saved
//! here is that obligation's, and only that obligation's.
//!
//! Unlike a normal agent chat (which owns its own OS window, see
//! `app::InteractiveAgentWindowControl`), the chat here is constructed
//! in-process via `InteractiveAgentView::with_embedded(true)` so it can sit
//! side by side with the mockup preview in one drawer panel, mirroring the
//! `ObligationsView`/`LifecyclePanelView` drawer convention (see CLAUDE.md's
//! GPUI keyboard-focus section).
//!
//! The mockup preview polls the obligation's `visual_design_path` column
//! (written by `tod-cli visual-design save`) the same way `ObligationsView`
//! polls `fleet.subscribe_changes()` for live updates, and reloads the
//! `WebView` when the path changes.

use crate::app::InteractiveAgentWindowControl;
use crate::interview::agent::SharedAgent;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use crate::ui::panel_split::{PanelSplitState, h_panel_split};
use crate::views::interactive_agent::InteractiveAgentView;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, Styled, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use gpui_wry::WebView;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tod_store::TodSettings;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

const VISUAL_DESIGN_PANEL_CONTEXT: &str = "VisualDesignPanel";
const POLL_INTERVAL: Duration = Duration::from_millis(400);

actions!(visual_design_panel, [VisualDesignPanelClose]);

#[derive(Debug, Clone)]
pub enum VisualDesignPanelEvent {
    Close,
    /// Ctrl+Left — move keyboard focus back to the task tree, leaving the
    /// panel open (mirrors the drawer-panel convention in CLAUDE.md).
    FocusTaskList,
}

/// Everything needed to construct the embedded chat, assembled by the shell
/// through `InteractiveAgentWindowControl::create_embedded_session`.
pub struct EmbeddedChatParams {
    /// Node the chat is launched from.
    pub node_id: String,
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
    /// The obligation this panel session is scoped to — its mockup, and only
    /// its mockup, is shown and saved here.
    obligation_id: Option<Uuid>,
    title: String,
    split: Entity<PanelSplitState>,
    webview: Option<Entity<WebView>>,
    /// A URL waiting to become a `WebView` — deferred because building one
    /// needs a real `Window`, which the poll task that discovers a path
    /// change does not have.
    pending_webview_url: Option<String>,
    chat: Option<Entity<InteractiveAgentView>>,
    /// The `visual_design_path` currently loaded, so a change can be
    /// detected without re-querying the obligation on every poll tick.
    loaded_path: Option<String>,
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
                cx.background_executor().timer(POLL_INTERVAL).await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if !changed {
                    continue;
                }
                let Ok(()) = poll_entity.update(cx, |this, cx| this.reload_if_changed(cx)) else {
                    break;
                };
            }
        });

        Self {
            fleet,
            node_id: None,
            obligation_id: None,
            title: String::new(),
            split: cx.new(|_| PanelSplitState::centered()),
            webview: None,
            pending_webview_url: None,
            chat: None,
            loaded_path: None,
            focus_handle: cx.focus_handle(),
            _poll_task,
        }
    }

    pub fn is_open(&self) -> bool {
        self.obligation_id.is_some()
    }

    /// The node whose obligation this panel session is scoped to.
    pub fn node_id(&self) -> Option<Uuid> {
        self.node_id
    }

    /// Open the panel scoped to `obligation_id` on `node_id`, constructing
    /// the embedded chat fresh — sessions are never reused, mirroring every
    /// other agent-chat entry point in the app.
    pub fn open(
        &mut self,
        node_id: Uuid,
        obligation_id: Uuid,
        title: &str,
        chat: EmbeddedChatParams,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.node_id = Some(node_id);
        self.obligation_id = Some(obligation_id);
        self.title = title.to_string();
        self.loaded_path = None;
        self.webview = None;
        self.pending_webview_url = None;

        let engagement = chat.window_control.engagement();
        let view = cx.new(|cx| {
            InteractiveAgentView::new(
                chat.node_id,
                chat.session_run_id,
                chat.fleet,
                chat.agent,
                chat.workspace_cwd,
                chat.window_control,
                chat.initial_context,
                None,
                chat.settings,
                engagement,
                window,
                cx,
            )
            .with_embedded(true)
        });
        self.chat = Some(view);

        self.reload_if_changed(cx);
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.obligation_id.is_none() {
            return;
        }
        self.node_id = None;
        self.obligation_id = None;
        self.title.clear();
        self.chat = None;
        self.webview = None;
        self.pending_webview_url = None;
        self.loaded_path = None;
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

    /// Look up this panel's obligation's `visual_design_path` and, if it
    /// changed, (re)create the webview pointed at the new file.
    fn reload_if_changed(&mut self, cx: &mut Context<Self>) {
        let Some(obligation_id) = self.obligation_id else {
            return;
        };
        let Some(path) = self
            .fleet
            .get_obligation(obligation_id)
            .ok()
            .flatten()
            .and_then(|o| o.visual_design_path)
        else {
            return;
        };
        if Some(&path) == self.loaded_path.as_ref() {
            return;
        }
        self.loaded_path = Some(path.clone());
        let url = path_to_file_url(Path::new(&path));
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
