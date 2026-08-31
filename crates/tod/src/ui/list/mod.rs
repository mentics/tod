mod keyboard;

pub use keyboard::{
    ListArrowDown, ListArrowUp, ListEnd, ListHome, ListPageDown, ListPageUp,
    register_list_keyboard_bindings,
};

use gpui::{App, Entity, Pixels, Styled, Window, px};
use gpui_component::list::{List, ListDelegate, ListState};

/// Uniform two-line task row height used for viewport/page calculations.
pub const TWO_LINE_ROW_HEIGHT: Pixels = px(44.);

/// Approximate visible row count from list viewport height.
pub fn viewport_row_count(viewport_height: Pixels) -> usize {
    (viewport_height / TWO_LINE_ROW_HEIGHT).floor().max(1.) as usize
}

/// Thin wrapper around gpui-component `List` + `ListState`.
pub struct ListView<D: ListDelegate + 'static> {
    state: Entity<ListState<D>>,
}

impl<D: ListDelegate + 'static> ListView<D> {
    pub fn new(state: Entity<ListState<D>>) -> Self {
        Self { state }
    }

    pub fn render(&self, _window: &mut Window, _cx: &mut App) -> List<D> {
        List::new(&self.state).size_full()
    }
}
