//! `WindowMover`: finds, moves and focuses the design window per OS.
//! Design: `doc/ui/visual-design-browser.md` section 6.5.

use super::placement::NativeRect;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
pub mod windows;

/// An OS window found by title. `id` is the native handle (HWND on Windows,
/// X11 window id on Linux); on macOS it is unused and the title is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowHandle {
    pub id: u64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoverError {
    /// This OS or session cannot move windows (Wayland, tool missing).
    Unsupported(String),
    /// The OS refused; the message says where to grant it.
    PermissionDenied(String),
    /// The window is gone or the call failed.
    Failed(String),
}

impl std::fmt::Display for MoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(m) => write!(f, "window moving is not supported: {m}"),
            Self::PermissionDenied(m) => write!(f, "permission denied: {m}"),
            Self::Failed(m) => write!(f, "window move failed: {m}"),
        }
    }
}

impl std::error::Error for MoverError {}

pub trait WindowMover: Send + Sync {
    /// Find our window by its unique title; None if not open.
    fn find(&self, title_prefix: &str) -> Option<WindowHandle>;
    fn move_to(&self, window: &WindowHandle, rect: NativeRect) -> Result<(), MoverError>;
    fn focus(&self, window: &WindowHandle) -> Result<(), MoverError>;
}

/// The mover for this OS.
pub fn system_mover() -> Box<dyn WindowMover> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsMover)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacMover)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxMover)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        Box::new(UnsupportedMover)
    }
}

/// Reports `Unsupported` for everything.
pub struct UnsupportedMover;

impl WindowMover for UnsupportedMover {
    fn find(&self, _: &str) -> Option<WindowHandle> {
        None
    }
    fn move_to(&self, _: &WindowHandle, _: NativeRect) -> Result<(), MoverError> {
        Err(MoverError::Unsupported("no window mover on this OS".into()))
    }
    fn focus(&self, _: &WindowHandle) -> Result<(), MoverError> {
        Err(MoverError::Unsupported("no window mover on this OS".into()))
    }
}

/// Test double: holds one window, records moves and focuses.
#[derive(Default)]
pub struct FakeMover {
    state: std::sync::Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    window: Option<WindowHandle>,
    moves: Vec<NativeRect>,
    focuses: usize,
    error: Option<MoverError>,
}

impl FakeMover {
    pub fn with_window(title: &str) -> Self {
        let m = Self::default();
        m.open(title);
        m
    }
    pub fn open(&self, title: &str) {
        self.state.lock().unwrap().window = Some(WindowHandle { id: 1, title: title.into() });
    }
    pub fn close(&self) {
        self.state.lock().unwrap().window = None;
    }
    /// Make every move and focus fail with this error.
    pub fn fail_with(&self, e: Option<MoverError>) {
        self.state.lock().unwrap().error = e;
    }
    pub fn moves(&self) -> Vec<NativeRect> {
        self.state.lock().unwrap().moves.clone()
    }
    pub fn focus_count(&self) -> usize {
        self.state.lock().unwrap().focuses
    }
}

impl WindowMover for FakeMover {
    fn find(&self, prefix: &str) -> Option<WindowHandle> {
        let s = self.state.lock().unwrap();
        s.window.clone().filter(|w| w.title.starts_with(prefix))
    }
    fn move_to(&self, _: &WindowHandle, rect: NativeRect) -> Result<(), MoverError> {
        let mut s = self.state.lock().unwrap();
        if let Some(e) = s.error.clone() {
            return Err(e);
        }
        if s.window.is_none() {
            return Err(MoverError::Failed("window closed".into()));
        }
        s.moves.push(rect);
        Ok(())
    }
    fn focus(&self, _: &WindowHandle) -> Result<(), MoverError> {
        let mut s = self.state.lock().unwrap();
        if let Some(e) = s.error.clone() {
            return Err(e);
        }
        s.focuses += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_finds_moves_and_focuses() {
        let m = FakeMover::with_window("tod design abc");
        assert!(m.find("other").is_none());
        let w = m.find("tod design").unwrap();
        let r = NativeRect { x: 1, y: 2, w: 3, h: 4 };
        m.move_to(&w, r).unwrap();
        m.focus(&w).unwrap();
        assert_eq!(m.moves(), vec![r]);
        assert_eq!(m.focus_count(), 1);
        m.fail_with(Some(MoverError::PermissionDenied("x".into())));
        assert!(matches!(m.move_to(&w, r), Err(MoverError::PermissionDenied(_))));
        m.close();
        assert!(m.find("tod design").is_none());
    }

    #[test]
    fn unsupported_mover_reports_unsupported() {
        let m = UnsupportedMover;
        let w = WindowHandle { id: 0, title: String::new() };
        assert!(matches!(m.focus(&w), Err(MoverError::Unsupported(_))));
    }
}
