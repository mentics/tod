//! Single-instance command history window — open, focus, close.

use crate::fleet::FleetStore;
use crate::views::command_history::CommandHistoryView;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::{Root, TitleBar};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryWindowStatus {
    Closed,
    Open,
}

/// Shared handle for the optional command-history window.
#[derive(Clone)]
pub struct HistoryWindowControl {
    handle: Arc<Mutex<Option<AnyWindowHandle>>>,
    fleet: Arc<Mutex<Option<Arc<FleetStore>>>>,
}

impl HistoryWindowControl {
    pub fn new() -> Self {
        Self {
            handle: Arc::new(Mutex::new(None)),
            fleet: Arc::new(Mutex::new(None)),
        }
    }

    pub fn bind(&self, fleet: Arc<FleetStore>) {
        *self.fleet.lock().expect("history window fleet mutex") = Some(fleet);
    }

    pub fn status(&self, cx: &mut App) -> HistoryWindowStatus {
        if self.live_handle(cx).is_some() {
            HistoryWindowStatus::Open
        } else {
            HistoryWindowStatus::Closed
        }
    }

    pub fn clear(&self) {
        *self.handle.lock().expect("history window handle mutex") = None;
    }

    fn set_handle(&self, handle: AnyWindowHandle) {
        *self.handle.lock().expect("history window handle mutex") = Some(handle);
    }

    fn live_handle(&self, cx: &mut App) -> Option<AnyWindowHandle> {
        let mut guard = self.handle.lock().expect("history window handle mutex");
        if let Some(handle) = *guard {
            if handle.update(cx, |_, _, _| Ok::<(), String>(())).is_ok() {
                return Some(handle);
            }
            *guard = None;
        }
        None
    }

    fn focus_handle(&self, cx: &mut App, handle: AnyWindowHandle) -> Result<(), String> {
        handle
            .update(cx, |_, window, _| {
                window.activate_window();
                Ok(())
            })
            .map_err(|err| format!("focus history window failed: {err}"))?
    }

    pub fn open_or_focus(&self, cx: &mut App) -> Result<(), String> {
        if let Some(handle) = self.live_handle(cx) {
            return self.focus_handle(cx, handle);
        }

        let fleet = self
            .fleet
            .lock()
            .expect("history window fleet mutex")
            .clone()
            .ok_or_else(|| "history window not bound to fleet store".to_string())?;

        let control = self.clone();
        let opened = cx
            .open_window(
                WindowOptions {
                    titlebar: Some(TitleBar::title_bar_options()),
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(120.), px(120.)),
                        size: size(px(640.), px(480.)),
                    })),
                    ..Default::default()
                },
                move |window, cx| {
                    let control_for_close = control.clone();
                    window.on_window_should_close(cx, move |_, _| {
                        control_for_close.clear();
                        true
                    });
                    let view = cx.new(|cx| {
                        CommandHistoryView::new(window, cx, fleet, control.clone())
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .map_err(|err| format!("open history window failed: {err}"))?;

        self.set_handle(opened.into());
        Ok(())
    }

    pub fn close(&self, cx: &mut App) {
        if let Some(handle) = self.live_handle(cx) {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.clear();
    }
}
