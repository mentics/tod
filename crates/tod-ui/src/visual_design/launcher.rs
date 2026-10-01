//! Opens or re-docks the design window (design section 6.1).
//!
//! One window per profile. Open-or-re-dock is decided by the server's
//! `connected` first and the mover's `find` second; a window that exists is
//! never launched again. Every method here blocks (discovery, process spawn,
//! OS window calls): call them from a background thread, never the UI thread.
//! State changes are sent on the channel returned by [`Launcher::new`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::browser::{self, Browser, BrowserError};
use super::mover::{system_mover, MoverError, WindowHandle, WindowMover};
use super::placement::{
    display_for, dock_rect, to_native_units, NativeRect, Platform, Rect, DEFAULT_MIN_WIDTH,
};
use super::server::DesignServer;

/// How long the event stream may stay dropped before the window counts as closed.
pub const CLOSE_GRACE: Duration = Duration::from_secs(4);

/// What the side pane listens to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LauncherEvent {
    Opened,
    Closed,
    Redocked,
    ChromeMissing(String),
    MoverUnsupported(String),
    PermissionDenied(String),
    Failed(String),
}

/// tod's window and displays, as plain numbers read on the UI thread.
#[derive(Debug, Clone)]
pub struct Dock {
    /// tod's bounds in logical pixels.
    pub tod: Rect,
    /// Every display's work area in logical pixels.
    pub work_areas: Vec<Rect>,
    pub scale_factor: f64,
}

impl Dock {
    fn logical(&self) -> Option<Rect> {
        let area = display_for(self.tod, &self.work_areas)?;
        Some(dock_rect(self.tod, area, DEFAULT_MIN_WIDTH))
    }
}

/// Starts the browser process. Injectable for tests.
pub trait Spawner: Send + Sync {
    fn spawn(&self, exe: &Path, args: &[OsString]) -> std::io::Result<()>;
}

pub struct ProcessSpawner;

impl Spawner for ProcessSpawner {
    fn spawn(&self, exe: &Path, args: &[OsString]) -> std::io::Result<()> {
        let mut child = std::process::Command::new(exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        // Reap it when it exits; the window outlives this call.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

struct Live {
    url: String,
    title_prefix: String,
    /// The window was seen connected at least once.
    seen_connected: bool,
    disconnected_since: Option<Instant>,
}

pub struct Launcher {
    profile: PathBuf,
    browsers: Vec<Box<dyn Browser>>,
    mover: Arc<dyn WindowMover>,
    spawner: Arc<dyn Spawner>,
    events: Sender<LauncherEvent>,
    /// Discovery runs once; (index into `browsers`, executable) or the error.
    discovered: Mutex<Option<Result<(usize, PathBuf), BrowserError>>>,
    server: Mutex<Option<DesignServer>>,
    /// Given to every server this launcher starts.
    feedback: Mutex<Option<super::server::FeedbackHandler>>,
    live: Mutex<Option<Live>>,
}

impl Launcher {
    /// The real launcher for `data_root`, honouring `visual_design.browser`.
    pub fn system(
        data_root: &Path,
        browser_override: Option<PathBuf>,
    ) -> (Self, Receiver<LauncherEvent>) {
        Self::new(
            data_root,
            browser::registry_with_override(browser_override),
            Arc::from(system_mover()),
            Arc::new(ProcessSpawner),
        )
    }

    pub fn new(
        data_root: &Path,
        browsers: Vec<Box<dyn Browser>>,
        mover: Arc<dyn WindowMover>,
        spawner: Arc<dyn Spawner>,
    ) -> (Self, Receiver<LauncherEvent>) {
        let (tx, rx) = channel();
        let l = Self {
            profile: data_root.join("visual-design").join("browser-profile"),
            browsers,
            mover,
            spawner,
            events: tx,
            discovered: Mutex::new(None),
            server: Mutex::new(None),
            feedback: Mutex::new(None),
            live: Mutex::new(None),
        };
        (l, rx)
    }

    /// Where feedback posted by the page goes (on a server thread); applies
    /// to the running server and to any started later.
    pub fn set_feedback_handler(&self, f: super::server::FeedbackHandler) {
        if let Some(s) = self.server.lock().unwrap().as_ref() {
            let f = f.clone();
            s.on_feedback(move |t, fb| f(t, fb));
        }
        *self.feedback.lock().unwrap() = Some(f);
    }

    pub fn profile_dir(&self) -> &Path {
        &self.profile
    }

    fn emit(&self, e: LauncherEvent) {
        let _ = self.events.send(e);
    }

    fn browser(&self) -> Result<(&dyn Browser, PathBuf), BrowserError> {
        let mut cache = self.discovered.lock().unwrap();
        if cache.is_none() {
            *cache = Some(match browser::first_available(&self.browsers) {
                Ok((b, p)) => {
                    let i = self.browsers.iter().position(|x| x.id() == b.id()).unwrap_or(0);
                    Ok((i, p))
                }
                Err(e) => Err(e),
            });
        }
        match cache.as_ref().unwrap() {
            Ok((i, p)) => Ok((self.browsers[*i].as_ref(), p.clone())),
            Err(e) => Err(e.clone()),
        }
    }

    fn report_mover(&self, e: MoverError) {
        self.emit(match e {
            MoverError::Unsupported(m) => LauncherEvent::MoverUnsupported(m),
            MoverError::PermissionDenied(m) => LauncherEvent::PermissionDenied(m),
            MoverError::Failed(m) => LauncherEvent::Failed(m),
        });
    }

    /// Opens the design window on `mockup`, or re-docks and focuses the one
    /// that is already open. Returns true if a window is now showing.
    pub fn open_or_redock(&self, mockup: &Path, dock: &Dock) -> bool {
        if let Some((url, prefix)) = self.current() {
            if self.window_exists(&url, &prefix) {
                if let Some(s) = self.server.lock().unwrap().as_ref() {
                    s.set_path(&url, mockup);
                }
                return self.redock(&prefix, dock);
            }
            // Session left over from a window that is gone.
            self.end_session(true);
        }
        self.launch(mockup, dock)
    }

    /// Points the open window at another mockup without moving it; false when
    /// no window is open.
    pub fn navigate(&self, mockup: &Path) -> bool {
        let Some((url, _)) = self.current() else { return false };
        match self.server.lock().unwrap().as_ref() {
            Some(s) => {
                s.set_path(&url, mockup);
                true
            }
            None => false,
        }
    }

    /// Re-docks an open window without changing its page.
    pub fn redock_only(&self, dock: &Dock) -> bool {
        match self.current() {
            Some((_, prefix)) => self.redock(&prefix, dock),
            None => false,
        }
    }

    fn current(&self) -> Option<(String, String)> {
        self.live
            .lock()
            .unwrap()
            .as_ref()
            .map(|l| (l.url.clone(), l.title_prefix.clone()))
    }

    fn window_exists(&self, url: &str, prefix: &str) -> bool {
        let connected = self
            .server
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.connected(url));
        connected || self.mover.find(prefix).is_some()
    }

    fn redock(&self, prefix: &str, dock: &Dock) -> bool {
        let Some(rect) = dock.logical() else {
            self.emit(LauncherEvent::Failed("no display to dock on".into()));
            return false;
        };
        let native = to_native_units(rect, dock.scale_factor, Platform::current());
        // Handles are opaque and may be stale: find again before every move.
        let Some(handle) = self.mover.find(prefix) else {
            self.emit(LauncherEvent::Failed("the design window was not found".into()));
            return false;
        };
        self.move_and_focus(&handle, native)
    }

    fn move_and_focus(&self, handle: &WindowHandle, native: NativeRect) -> bool {
        let r = self
            .mover
            .move_to(handle, native)
            .and_then(|_| self.mover.focus(handle));
        match r {
            Ok(()) => {
                self.emit(LauncherEvent::Redocked);
                true
            }
            Err(e) => {
                self.report_mover(e);
                false
            }
        }
    }

    fn launch(&self, mockup: &Path, dock: &Dock) -> bool {
        let (browser, exe) = match self.browser() {
            Ok(b) => b,
            Err(e) => {
                self.emit(LauncherEvent::ChromeMissing(e.to_string()));
                return false;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&self.profile) {
            self.emit(LauncherEvent::Failed(format!(
                "cannot create the browser profile: {e}"
            )));
            return false;
        }
        let url = {
            let mut server = self.server.lock().unwrap();
            if server.is_none() {
                match DesignServer::start() {
                    Ok(s) => {
                        if let Some(f) = self.feedback.lock().unwrap().clone() {
                            s.on_feedback(move |t, fb| f(t, fb));
                        }
                        *server = Some(s)
                    }
                    Err(e) => {
                        drop(server);
                        self.emit(LauncherEvent::Failed(format!(
                            "cannot start the design server: {e}"
                        )));
                        return false;
                    }
                }
            }
            server.as_ref().unwrap().open(mockup)
        };
        // Chrome's launch flags take the logical rect unchanged.
        let placement = dock.logical().map(|r| NativeRect {
            x: r.x.round() as i32,
            y: r.y.round() as i32,
            w: r.w.round() as i32,
            h: r.h.round() as i32,
        });
        let args = browser.launch_args(&url, &self.profile, placement);
        if let Err(e) = self.spawner.spawn(&exe, &args) {
            self.end_session_for(&url);
            self.emit(LauncherEvent::Failed(format!(
                "cannot start {}: {e}",
                browser.id()
            )));
            return false;
        }
        let token = url
            .split("/d/")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap_or("");
        *self.live.lock().unwrap() = Some(Live {
            url: url.clone(),
            title_prefix: format!("tod design {}", &token[..token.len().min(8)]),
            seen_connected: false,
            disconnected_since: None,
        });
        self.emit(LauncherEvent::Opened);
        true
    }

    /// Call periodically (off the UI thread). Detects a closed window by the
    /// event stream staying dropped for [`CLOSE_GRACE`], not by process exit.
    pub fn tick(&self, now: Instant) {
        let Some((url, _)) = self.current() else { return };
        let connected = self
            .server
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.connected(&url));
        let closed = {
            let mut guard = self.live.lock().unwrap();
            let Some(l) = guard.as_mut() else { return };
            if connected {
                l.seen_connected = true;
                l.disconnected_since = None;
                false
            } else if l.seen_connected {
                let since = *l.disconnected_since.get_or_insert(now);
                now.duration_since(since) >= CLOSE_GRACE
            } else {
                false
            }
        };
        if closed {
            self.end_session(true);
        }
    }

    /// Drops the session (the page is told to close) and frees the server.
    pub fn close_window(&self) {
        self.end_session(true);
    }

    fn end_session(&self, announce: bool) {
        let Some(l) = self.live.lock().unwrap().take() else { return };
        self.end_session_for(&l.url);
        if announce {
            self.emit(LauncherEvent::Closed);
        }
    }

    /// Closes the session and drops the server when none remain.
    fn end_session_for(&self, url: &str) {
        let mut server = self.server.lock().unwrap();
        if let Some(s) = server.as_ref() {
            s.close(url);
            if s.session_count() == 0 {
                *server = None;
            }
        }
    }

    pub fn has_server(&self) -> bool {
        self.server.lock().unwrap().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::super::browser::{BrowserRect, Capabilities};
    use super::super::mover::FakeMover;
    use super::*;

    struct FakeBrowser(Option<PathBuf>);
    impl Browser for FakeBrowser {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn discover(&self) -> Option<PathBuf> {
            self.0.clone()
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities { app_mode: true, launch_placement: true, separate_profile: true }
        }
        fn launch_args(&self, url: &str, _: &Path, p: Option<BrowserRect>) -> Vec<OsString> {
            vec![url.into(), format!("{p:?}").into()]
        }
    }

    #[derive(Default)]
    struct FakeSpawner(Mutex<Vec<Vec<OsString>>>);
    impl Spawner for FakeSpawner {
        fn spawn(&self, _: &Path, args: &[OsString]) -> std::io::Result<()> {
            self.0.lock().unwrap().push(args.to_vec());
            Ok(())
        }
    }

    fn dock() -> Dock {
        Dock {
            tod: Rect::new(0.0, 0.0, 800.0, 900.0),
            work_areas: vec![Rect::new(0.0, 0.0, 1920.0, 1040.0)],
            scale_factor: 1.5,
        }
    }

    type Setup = (
        Launcher,
        Receiver<LauncherEvent>,
        Arc<FakeMover>,
        Arc<FakeSpawner>,
        PathBuf,
    );

    fn setup(exe: Option<&str>) -> Setup {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("tod-launcher-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mockup = dir.join("m.html");
        std::fs::write(&mockup, "<html><body>x</body></html>").unwrap();
        let mover = Arc::new(FakeMover::default());
        let spawner = Arc::new(FakeSpawner::default());
        let (l, rx) = Launcher::new(
            &dir,
            vec![Box::new(FakeBrowser(exe.map(PathBuf::from)))],
            mover.clone(),
            spawner.clone(),
        );
        (l, rx, mover, spawner, mockup)
    }

    fn drain(rx: &Receiver<LauncherEvent>) -> Vec<LauncherEvent> {
        rx.try_iter().collect()
    }

    #[test]
    fn launches_once_then_redocks() {
        let (l, rx, mover, spawner, m) = setup(Some("chrome"));
        assert!(l.open_or_redock(&m, &dock()));
        assert_eq!(drain(&rx), vec![LauncherEvent::Opened]);
        assert_eq!(spawner.0.lock().unwrap().len(), 1);
        let prefix = l.current().unwrap().1;
        mover.open(&format!("{prefix} - Chrome"));
        assert!(l.open_or_redock(&m, &dock()));
        assert_eq!(drain(&rx), vec![LauncherEvent::Redocked]);
        assert_eq!(spawner.0.lock().unwrap().len(), 1, "no duplicate window");
        assert_eq!(mover.moves().len(), 1);
        assert_eq!(mover.focus_count(), 1);
    }

    #[test]
    fn closed_then_reopen_launches_again() {
        let (l, rx, mover, spawner, m) = setup(Some("chrome"));
        l.open_or_redock(&m, &dock());
        drain(&rx);
        // Never connected and no window: the stale session is replaced.
        assert!(l.open_or_redock(&m, &dock()));
        assert_eq!(drain(&rx), vec![LauncherEvent::Closed, LauncherEvent::Opened]);
        assert_eq!(spawner.0.lock().unwrap().len(), 2);
        assert!(mover.moves().is_empty());
    }

    #[test]
    fn dropped_stream_closes_after_grace_and_frees_server() {
        let (l, rx, _mover, _s, m) = setup(Some("chrome"));
        l.open_or_redock(&m, &dock());
        drain(&rx);
        l.live.lock().unwrap().as_mut().unwrap().seen_connected = true;
        let t0 = Instant::now();
        l.tick(t0);
        assert!(drain(&rx).is_empty());
        l.tick(t0 + CLOSE_GRACE + Duration::from_secs(1));
        assert_eq!(drain(&rx), vec![LauncherEvent::Closed]);
        assert!(!l.has_server());
    }

    #[test]
    fn chrome_missing_is_reported_and_nothing_spawns() {
        let (l, rx, _m, spawner, mockup) = setup(None);
        assert!(!l.open_or_redock(&mockup, &dock()));
        assert!(matches!(drain(&rx)[0], LauncherEvent::ChromeMissing(_)));
        assert!(spawner.0.lock().unwrap().is_empty());
        assert!(!l.has_server());
    }

    #[test]
    fn mover_errors_map_to_events() {
        let (l, rx, mover, _s, m) = setup(Some("chrome"));
        l.open_or_redock(&m, &dock());
        let prefix = l.current().unwrap().1;
        mover.open(&prefix);
        drain(&rx);
        mover.fail_with(Some(MoverError::Unsupported("wayland".into())));
        assert!(!l.open_or_redock(&m, &dock()));
        mover.fail_with(Some(MoverError::PermissionDenied("grant it".into())));
        assert!(!l.open_or_redock(&m, &dock()));
        assert_eq!(
            drain(&rx),
            vec![
                LauncherEvent::MoverUnsupported("wayland".into()),
                LauncherEvent::PermissionDenied("grant it".into()),
            ]
        );
    }

    #[test]
    fn close_window_ends_session() {
        let (l, rx, _m, _s, mk) = setup(Some("chrome"));
        l.open_or_redock(&mk, &dock());
        l.close_window();
        assert_eq!(drain(&rx), vec![LauncherEvent::Opened, LauncherEvent::Closed]);
        assert!(!l.has_server());
    }

    /// Real Chrome and window mover. Run by hand:
    /// `cargo test -p tod-ui --lib launcher::tests::real_run -- --ignored --nocapture`.
    /// Leaves its Chrome window open (profile in a temp dir); close it afterward.
    #[test]
    #[ignore]
    fn real_run() {
        let dir = std::env::temp_dir().join("tod-launcher-real");
        std::fs::create_dir_all(&dir).unwrap();
        let mockup = dir.join("m.html");
        std::fs::write(&mockup, "<html><body><h1>real run</h1></body></html>").unwrap();
        let (l, rx) = Launcher::system(&dir, None);
        let d = Dock {
            tod: Rect::new(0.0, 0.0, 800.0, 700.0),
            work_areas: vec![Rect::new(0.0, 0.0, 1600.0, 900.0)],
            scale_factor: 1.0,
        };
        assert!(l.open_or_redock(&mockup, &d), "{:?}", drain(&rx));
        std::thread::sleep(Duration::from_secs(5));
        l.tick(Instant::now());
        let d2 = Dock { tod: Rect::new(400.0, 0.0, 800.0, 700.0), ..d };
        let ok = l.open_or_redock(&mockup, &d2);
        println!("redock {ok}: {:?}", drain(&rx));
        assert!(ok);
    }
}
