//! The Tasks view's right-hand drawer: the one container for every panel that
//! shows the node selected in the task tree.
//!
//! Every panel follows the same contract — none is special:
//!
//! - **One at a time.** Opening a panel closes whichever other panel is open.
//! - **Follows the tree.** Whenever the tree selection changes, the open panel
//!   switches to the newly selected node (`follow`) without taking keyboard
//!   focus. With nothing selected the drawer closes.
//! - **Same controls.** Escape from the tree closes it, Ctrl+Right focuses it,
//!   and a panel closing itself hands focus back to the tree — all routed
//!   through here by the shell (`crate::app::window`).
//!
//! A new panel gets a `DrawerKind` variant and an arm in each method below;
//! the task list never learns which panel is showing.

use crate::views::agent_config_panel::AgentConfigPanelView;
use crate::views::lifecycle_panel::LifecyclePanelView;
use crate::views::obligations::ObligationsView;
use crate::views::task_edit::TaskEditView;
use crate::views::visual_design_panel::VisualDesignPanelView;
use gpui::{AnyElement, App, Entity, Focusable, IntoElement, Window};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrawerKind {
    TaskEdit,
    Obligations,
    Lifecycle,
    VisualDesign,
    AgentConfig,
}

const ALL_KINDS: [DrawerKind; 5] = [
    DrawerKind::TaskEdit,
    DrawerKind::Obligations,
    DrawerKind::Lifecycle,
    DrawerKind::VisualDesign,
    DrawerKind::AgentConfig,
];

/// A change to the drawer, queued from event handlers (which have no
/// `Window`) and applied in order on the shell's next render.
pub(crate) enum DrawerRequest {
    OpenTaskEdit {
        task_id: String,
    },
    OpenObligations {
        task_id: String,
        title: String,
    },
    OpenLifecycle {
        task_id: String,
    },
    OpenVisualDesign {
        node_id: Uuid,
        obligation_id: Uuid,
    },
    OpenAgentConfig {
        task_id: String,
        config_id: Option<String>,
        /// Launch an auto run once the panel is open.
        launch_auto: bool,
    },
    /// The tree selection changed.
    Follow {
        task_id: Option<String>,
    },
    Close,
    Focus,
}

pub(crate) struct RightDrawer {
    pub task_edit: Entity<TaskEditView>,
    pub obligations: Entity<ObligationsView>,
    pub lifecycle: Entity<LifecyclePanelView>,
    pub visual_design: Entity<VisualDesignPanelView>,
    pub agent_config: Entity<AgentConfigPanelView>,
}

impl RightDrawer {
    fn is_kind_open(&self, kind: DrawerKind, cx: &App) -> bool {
        match kind {
            DrawerKind::TaskEdit => self.task_edit.read(cx).is_open(),
            DrawerKind::Obligations => self.obligations.read(cx).is_open(),
            DrawerKind::Lifecycle => self.lifecycle.read(cx).is_open(),
            DrawerKind::VisualDesign => self.visual_design.read(cx).is_open(),
            DrawerKind::AgentConfig => self.agent_config.read(cx).is_open(),
        }
    }

    /// The panel currently showing, if any. Opening goes through
    /// `close_except`, so at most one is ever open.
    pub fn active(&self, cx: &App) -> Option<DrawerKind> {
        ALL_KINDS
            .into_iter()
            .find(|kind| self.is_kind_open(*kind, cx))
    }

    pub fn is_open(&self, cx: &App) -> bool {
        self.active(cx).is_some()
    }

    /// Close every open panel other than `keep` — call before opening `keep`.
    pub fn close_except(&self, keep: Option<DrawerKind>, window: &mut Window, cx: &mut App) {
        for kind in ALL_KINDS {
            if Some(kind) == keep || !self.is_kind_open(kind, cx) {
                continue;
            }
            match kind {
                DrawerKind::TaskEdit => self.task_edit.update(cx, |panel, cx| panel.close(cx)),
                DrawerKind::Obligations => {
                    self.obligations.update(cx, |panel, cx| panel.close(window, cx))
                }
                DrawerKind::Lifecycle => self.lifecycle.update(cx, |panel, cx| panel.close(cx)),
                DrawerKind::VisualDesign => {
                    self.visual_design.update(cx, |panel, cx| panel.close(cx))
                }
                DrawerKind::AgentConfig => {
                    self.agent_config.update(cx, |panel, cx| panel.close(cx))
                }
            }
        }
    }

    /// Switch the open panel to `task_id`, the newly selected tree node.
    /// Never moves keyboard focus: the user is navigating the tree.
    pub fn follow(&self, task_id: &str, fleet: &FleetStore, window: &mut Window, cx: &mut App) {
        let Some(kind) = self.active(cx) else {
            return;
        };
        match kind {
            DrawerKind::TaskEdit => {
                self.task_edit
                    .update(cx, |panel, cx| panel.retarget(task_id, window, cx));
            }
            DrawerKind::Obligations => {
                self.show_obligations(task_id, fleet, window, cx);
            }
            DrawerKind::Lifecycle => {
                self.lifecycle
                    .update(cx, |panel, cx| panel.retarget(task_id, cx));
            }
            DrawerKind::VisualDesign => {
                // A visual design session belongs to one obligation, which a
                // different node does not have — show that node's obligations,
                // the list design sessions are opened from.
                let same_node = Uuid::parse_str(task_id)
                    .is_ok_and(|id| self.visual_design.read(cx).node_id() == Some(id));
                if !same_node {
                    self.show_obligations(task_id, fleet, window, cx);
                    self.close_except(Some(DrawerKind::Obligations), window, cx);
                }
            }
            DrawerKind::AgentConfig => {
                // Show the node's first action config (possibly inherited, so
                // opened against its owner), or a new-config form if it has none.
                let first = fleet
                    .resolve_agents_for_node(task_id)
                    .ok()
                    .and_then(|resolved| {
                        resolved
                            .configs
                            .into_iter()
                            .find(|config| config.mode != "interview")
                    });
                self.agent_config.update(cx, |panel, cx| match first {
                    Some(config) => panel.retarget(&config.node_id, Some(&config.id), window, cx),
                    None => panel.retarget(task_id, None, window, cx),
                });
            }
        }
    }

    fn show_obligations(&self, task_id: &str, fleet: &FleetStore, window: &mut Window, cx: &mut App) {
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let title = fleet
            .get_node(task_id)
            .ok()
            .flatten()
            .map(|task| task.title)
            .unwrap_or_default();
        self.obligations.update(cx, |panel, cx| {
            panel.retarget(node_id, &title, None, false, window, cx);
        });
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        match self.active(cx) {
            Some(DrawerKind::TaskEdit) => self.task_edit.read(cx).focus_handle(cx).focus(window, cx),
            Some(DrawerKind::Obligations) => {
                self.obligations.read(cx).focus_handle(cx).focus(window, cx)
            }
            Some(DrawerKind::Lifecycle) => {
                self.lifecycle.update(cx, |panel, cx| panel.focus(window, cx))
            }
            Some(DrawerKind::VisualDesign) => {
                self.visual_design.read(cx).focus_handle(cx).focus(window, cx)
            }
            Some(DrawerKind::AgentConfig) => {
                self.agent_config.read(cx).focus_handle(cx).focus(window, cx)
            }
            None => {}
        }
    }

    pub fn element(&self, cx: &App) -> Option<AnyElement> {
        Some(match self.active(cx)? {
            DrawerKind::TaskEdit => self.task_edit.clone().into_any_element(),
            DrawerKind::Obligations => self.obligations.clone().into_any_element(),
            DrawerKind::Lifecycle => self.lifecycle.clone().into_any_element(),
            DrawerKind::VisualDesign => self.visual_design.clone().into_any_element(),
            DrawerKind::AgentConfig => self.agent_config.clone().into_any_element(),
        })
    }
}
