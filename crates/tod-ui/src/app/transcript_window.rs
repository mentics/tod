//! Single-instance agent transcript window — open, focus, close.

use crate::views::agent_transcripts::AgentTranscriptsView;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::{Root, TitleBar};
use std::sync::{Arc, Mutex};
use tod_store::agent_traffic::SharedAgentTrafficLog;
use tod_store::fleet::FleetStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptWindowStatus {
    Closed,
    Open,
}

/// Shared handle for the optional agent-transcript debug window.
#[derive(Clone)]
pub struct TranscriptWindowControl {
    handle: Arc<Mutex<Option<AnyWindowHandle>>>,
    fleet: Arc<Mutex<Option<Arc<FleetStore>>>>,
    traffic_log: Arc<Mutex<Option<SharedAgentTrafficLog>>>,
}

impl TranscriptWindowControl {
    pub fn new() -> Self {
        Self {
            handle: Arc::new(Mutex::new(None)),
            fleet: Arc::new(Mutex::new(None)),
            traffic_log: Arc::new(Mutex::new(None)),
        }
    }

    pub fn bind(&self, fleet: Arc<FleetStore>, traffic_log: SharedAgentTrafficLog) {
        *self.fleet.lock().expect("transcript window fleet mutex") = Some(fleet);
        *self
            .traffic_log
            .lock()
            .expect("transcript window traffic mutex") = Some(traffic_log);
    }

    pub fn status(&self, cx: &mut App) -> TranscriptWindowStatus {
        if self.live_handle(cx).is_some() {
            TranscriptWindowStatus::Open
        } else {
            TranscriptWindowStatus::Closed
        }
    }

    pub fn clear(&self) {
        *self.handle.lock().expect("transcript window handle mutex") = None;
    }

    fn set_handle(&self, handle: AnyWindowHandle) {
        *self.handle.lock().expect("transcript window handle mutex") = Some(handle);
    }

    fn live_handle(&self, cx: &mut App) -> Option<AnyWindowHandle> {
        let mut guard = self.handle.lock().expect("transcript window handle mutex");
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
            .map_err(|err| format!("focus transcript window failed: {err}"))?
    }

    /// Open the transcript window, or focus it if already open.
    pub fn open_or_focus(&self, cx: &mut App) -> Result<(), String> {
        if let Some(handle) = self.live_handle(cx) {
            return self.focus_handle(cx, handle);
        }

        let fleet = self
            .fleet
            .lock()
            .expect("transcript window fleet mutex")
            .clone()
            .ok_or_else(|| "transcript window not bound to fleet store".to_string())?;
        let traffic_log = self
            .traffic_log
            .lock()
            .expect("transcript window traffic mutex")
            .clone()
            .ok_or_else(|| "transcript window not bound to traffic log".to_string())?;

        let control = self.clone();
        let opened = cx
            .open_window(
                WindowOptions {
                    titlebar: Some(TitleBar::title_bar_options()),
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(80.), px(80.)),
                        size: size(px(1100.), px(720.)),
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
                        AgentTranscriptsView::new(window, cx, fleet, traffic_log, control.clone())
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .map_err(|err| format!("open transcript window failed: {err}"))?;

        self.set_handle(opened.into());
        Ok(())
    }

    /// Focus the transcript window if it is open.
    pub fn focus_if_open(&self, cx: &mut App) -> Result<(), String> {
        let Some(handle) = self.live_handle(cx) else {
            return Err("transcript window is not open".into());
        };
        self.focus_handle(cx, handle)
    }

    /// Close the transcript window if it is open.
    pub fn close(&self, cx: &mut App) -> Result<(), String> {
        let Some(handle) = self.live_handle(cx) else {
            return Ok(());
        };
        let _ = handle
            .update(cx, |_, window, _| {
                window.remove_window();
                Ok::<(), String>(())
            })
            .map_err(|err| format!("close transcript window failed: {err}"))?;
        self.clear();
        Ok(())
    }
}
