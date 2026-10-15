//! Single-instance log panel window — open, focus, close.

use crate::views::log_panel::LogPanelView;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::Root;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct LogWindowControl {
    handle: Arc<Mutex<Option<AnyWindowHandle>>>,
}

impl LogWindowControl {
    pub fn new() -> Self {
        Self::default()
    }

    fn clear(&self) {
        *self.handle.lock().expect("log window handle mutex") = None;
    }

    fn live_handle(&self, cx: &mut App) -> Option<AnyWindowHandle> {
        let mut guard = self.handle.lock().expect("log window handle mutex");
        if let Some(handle) = *guard {
            if handle.update(cx, |_, _, _| Ok::<(), String>(())).is_ok() {
                return Some(handle);
            }
            *guard = None;
        }
        None
    }

    pub fn open_or_focus(&self, cx: &mut App) -> Result<(), String> {
        if let Some(handle) = self.live_handle(cx) {
            crate::views::log_panel::mark_read();
            return handle
                .update(cx, |_, window, _| {
                    super::no_focus::activate(window);
                    Ok(())
                })
                .map_err(|err| format!("focus log window failed: {err}"))?;
        }
        let control = self.clone();
        let opened = cx
            .open_window(
                WindowOptions {
                    titlebar: Some(gpui_component::TitleBar::title_bar_options()),
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(140.), px(140.)),
                        size: size(px(900.), px(560.)),
                    })),
                    focus: super::no_focus::window_focus(),
                    ..super::app_icon::window_options()
                },
                move |window, cx| {
                    let control_for_close = control.clone();
                    window.on_window_should_close(cx, move |_, _| {
                        control_for_close.clear();
                        true
                    });
                    let view = cx.new(|cx| LogPanelView::new(window, cx, control.clone()));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .map_err(|err| format!("open log window failed: {err}"))?;
        *self.handle.lock().expect("log window handle mutex") = Some(opened.into());
        Ok(())
    }

    pub fn close(&self, cx: &mut App) {
        if let Some(handle) = self.live_handle(cx) {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.clear();
    }
}
