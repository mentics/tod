//! Two-panel horizontal split with a draggable divider.
//!
//! gpui-component's `h_resizable` drops `flex_none` after the first layout, so fixed-width
//! panels can shrink to nothing in a two-panel layout. This helper keeps the left pane at an
//! explicit width with `flex_shrink_0`.

use gpui::{
    App, AppContext, Bounds, Context, Element, ElementId, Empty, Entity, GlobalElementId,
    InteractiveElement, IntoElement, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, px,
};
use gpui_component::{ActiveTheme, h_flex};

const HANDLE_PADDING: Pixels = px(4.);
const HANDLE_SIZE: Pixels = px(1.);

#[derive(Debug, Clone)]
pub struct PanelSplitState {
    pub left_width: Pixels,
    pub(crate) dragging: bool,
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) initial_center: bool,
}

impl PanelSplitState {
    #[allow(dead_code)]
    pub fn new(left_width: Pixels) -> Self {
        Self {
            left_width,
            dragging: false,
            bounds: Bounds::default(),
            initial_center: false,
        }
    }

    pub fn centered() -> Self {
        Self {
            left_width: px(0.),
            dragging: false,
            bounds: Bounds::default(),
            initial_center: true,
        }
    }

    pub fn clamp_left_width(&mut self, min_left: Pixels, min_right: Pixels) {
        if self.initial_center {
            return;
        }
        let container = self.bounds.size.width;
        if container <= px(0.) {
            return;
        }
        let max_left = (container - min_right).max(min_left);
        self.left_width = self.left_width.clamp(min_left, max_left);
    }

    fn apply_initial_center(&mut self, min_left: Pixels, min_right: Pixels) -> bool {
        if !self.initial_center || self.bounds.size.width <= px(0.) {
            return false;
        }
        self.left_width = centered_left_width(self.bounds.size.width, min_left, min_right);
        self.initial_center = false;
        true
    }
}

fn centered_left_width(container: Pixels, min_left: Pixels, min_right: Pixels) -> Pixels {
    let max_left = (container - min_right).max(min_left);
    px(f32::from(container) / 2.).clamp(min_left, max_left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_left_width_uses_half_container() {
        let half = centered_left_width(px(1200.), px(240.), px(280.));
        assert_eq!(f32::from(half), 600.);
    }

    #[test]
    fn centered_left_width_respects_minimums() {
        let left = centered_left_width(px(400.), px(240.), px(280.));
        assert_eq!(f32::from(left), 240.);
    }
}

struct DragSplit;

impl Render for DragSplit {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(IntoElement)]
pub struct HPanelSplit {
    id: ElementId,
    state: Entity<PanelSplitState>,
    min_left: Pixels,
    min_right: Pixels,
    left: gpui::AnyElement,
    right: gpui::AnyElement,
}

pub fn h_panel_split(id: impl Into<ElementId>, state: &Entity<PanelSplitState>) -> HPanelSplit {
    HPanelSplit {
        id: id.into(),
        state: state.clone(),
        min_left: px(100.),
        min_right: px(100.),
        left: div().into_any_element(),
        right: div().into_any_element(),
    }
}

impl HPanelSplit {
    pub fn min_left(mut self, min: Pixels) -> Self {
        self.min_left = min;
        self
    }

    pub fn min_right(mut self, min: Pixels) -> Self {
        self.min_right = min;
        self
    }

    pub fn left(mut self, element: impl IntoElement) -> Self {
        self.left = element.into_any_element();
        self
    }

    pub fn right(mut self, element: impl IntoElement) -> Self {
        self.right = element.into_any_element();
        self
    }
}

impl RenderOnce for HPanelSplit {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state.clone();
        let min_left = self.min_left;
        let min_right = self.min_right;

        state.update(cx, |state, _| {
            state.clamp_left_width(min_left, min_right);
        });
        let handle_id = SharedString::from(format!("panel-split-handle-{}", self.id));
        let left_width = state.read(cx).left_width;

        h_flex()
            .id(self.id)
            .size_full()
            .min_w_0()
            .min_h_0()
            .child({
                canvas(
                    {
                        let state = state.clone();
                        move |bounds, _, cx| {
                            state.update(cx, |state, cx| {
                                state.bounds = bounds;
                                if state.apply_initial_center(min_left, min_right) {
                                    cx.notify();
                                }
                            })
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .child(
                div()
                    .h_full()
                    .w(left_width)
                    .flex_shrink_0()
                    .min_w(min_left)
                    .overflow_hidden()
                    .child(self.left),
            )
            .child(
                div()
                    .id(handle_id)
                    .occlude()
                    .flex_shrink_0()
                    .cursor_col_resize()
                    .h_full()
                    .w(HANDLE_SIZE)
                    .px(HANDLE_PADDING)
                    .bg(cx.theme().border)
                    .hover(|el| el.bg(cx.theme().drag_border))
                    .on_drag(DragSplit, {
                        let state = state.clone();
                        move |_, _, _, cx| {
                            state.update(cx, |state, _| state.dragging = true);
                            cx.new(|_| DragSplit)
                        }
                    }),
            )
            .child(
                div()
                    .h_full()
                    .flex_1()
                    .min_w(min_right)
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.right),
            )
            .child(PanelSplitDragElement {
                state,
                min_left,
                min_right,
            })
    }
}

struct PanelSplitDragElement {
    state: Entity<PanelSplitState>,
    min_left: Pixels,
    min_right: Pixels,
}

impl IntoElement for PanelSplitDragElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PanelSplitDragElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        (window.request_layout(gpui::Style::default(), None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let dragging = self.state.read(cx).dragging;
        if !dragging {
            return;
        }

        window.on_mouse_event({
            let state = self.state.clone();
            let min_left = self.min_left;
            let min_right = self.min_right;
            move |e: &MouseMoveEvent, phase, _, cx| {
                if !phase.bubble() {
                    return;
                }
                state.update(cx, |state, cx| {
                    let container = state.bounds.size.width;
                    let max_left = (container - min_right).max(min_left);
                    state.left_width =
                        (e.position.x - state.bounds.left()).clamp(min_left, max_left);
                    cx.notify();
                });
            }
        });

        window.on_mouse_event({
            let state = self.state.clone();
            move |_: &MouseUpEvent, phase, _, cx| {
                if phase.bubble() {
                    state.update(cx, |state, cx| {
                        state.dragging = false;
                        cx.notify();
                    });
                }
            }
        });
    }
}
