//! Windows mover: `EnumWindows` + `GetWindowTextW`, `SetWindowPos`.

use super::{MoverError, WindowHandle, WindowMover};
use crate::visual_design::placement::NativeRect;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsIconic, IsWindowVisible,
    PostMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_HIDE, SW_RESTORE, SW_SHOW, WM_CLOSE,
};

pub struct WindowsMover;

struct Search {
    prefix: String,
    /// A visible match wins; a hidden one is kept as the fallback.
    found: Option<WindowHandle>,
    hidden: Option<WindowHandle>,
}

unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let search = &mut *(lparam.0 as *mut Search);
        let n = GetWindowTextLengthW(hwnd);
        if n > 0 {
            let mut buf = vec![0u16; n as usize + 1];
            let got = GetWindowTextW(hwnd, &mut buf);
            let title = String::from_utf16_lossy(&buf[..got.max(0) as usize]);
            if title.starts_with(&search.prefix) {
                let handle = WindowHandle { id: hwnd.0 as usize as u64, title };
                if IsWindowVisible(hwnd).as_bool() {
                    search.found = Some(handle);
                    return BOOL(0);
                }
                search.hidden.get_or_insert(handle);
            }
        }
    }
    BOOL(1)
}

fn hwnd(w: &WindowHandle) -> HWND {
    HWND(w.id as usize as isize)
}

impl WindowsMover {
    /// The window's outer rectangle in screen pixels.
    pub fn is_visible(&self, w: &WindowHandle) -> bool {
        unsafe { IsWindowVisible(hwnd(w)).as_bool() }
    }

    pub fn rect(&self, w: &WindowHandle) -> Option<NativeRect> {
        let mut r = RECT::default();
        unsafe { GetWindowRect(hwnd(w), &mut r).ok()? };
        Some(NativeRect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
    }
}

impl WindowMover for WindowsMover {
    fn find(&self, title_prefix: &str) -> Option<WindowHandle> {
        let mut s = Search { prefix: title_prefix.to_string(), found: None, hidden: None };
        // EnumWindows reports an error when the callback stops it early.
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut s as *mut Search as isize)) };
        // Hidden windows (ours, after `hide`) are found too.
        s.found.or(s.hidden)
    }

    fn move_to(&self, window: &WindowHandle, r: NativeRect) -> Result<(), MoverError> {
        unsafe {
            if IsIconic(hwnd(window)).as_bool() {
                let _ = ShowWindow(hwnd(window), SW_RESTORE);
            }
            SetWindowPos(hwnd(window), HWND(0), r.x, r.y, r.w, r.h, SWP_NOZORDER | SWP_NOACTIVATE)
                .map_err(|e| MoverError::Failed(e.to_string()))
        }
    }

    fn hide(&self, window: &WindowHandle) -> Result<(), MoverError> {
        unsafe {
            let _ = ShowWindow(hwnd(window), SW_HIDE);
        }
        Ok(())
    }

    fn show(&self, window: &WindowHandle) -> Result<(), MoverError> {
        unsafe {
            let _ = ShowWindow(hwnd(window), SW_SHOW);
            if IsIconic(hwnd(window)).as_bool() {
                let _ = ShowWindow(hwnd(window), SW_RESTORE);
            }
        }
        Ok(())
    }

    fn close(&self, window: &WindowHandle) -> Result<(), MoverError> {
        unsafe {
            PostMessageW(hwnd(window), WM_CLOSE, WPARAM(0), LPARAM(0))
                .map_err(|e| MoverError::Failed(e.to_string()))
        }
    }

    fn focus(&self, window: &WindowHandle) -> Result<(), MoverError> {
        unsafe {
            if SetForegroundWindow(hwnd(window)).as_bool() {
                Ok(())
            } else {
                Err(MoverError::Failed("SetForegroundWindow was refused".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Real Chrome window check; run with `-- --ignored`.
    #[test]
    #[ignore]
    fn moves_a_real_chrome_window() {
        let chrome = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
        let dir = std::env::temp_dir().join(format!("tod-mover-{}", std::process::id()));
        let page = dir.join("p.html");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&page, "<html><head><title>tod mover check</title></head><body>x</body></html>").unwrap();
        let mut child = Command::new(chrome)
            .arg(format!("--app=file:///{}", page.display().to_string().replace('\\', "/")))
            .arg(format!("--user-data-dir={}", dir.join("profile").display()))
            .args(["--no-first-run", "--no-default-browser-check", "--window-position=100,100", "--window-size=700,500"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let m = WindowsMover;
        let mut found = None;
        for _ in 0..40 {
            found = m.find("tod mover check");
            if found.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let w = found.expect("window found");
        let before = m.rect(&w).unwrap();
        let target = NativeRect { x: 220, y: 180, w: 640, h: 480 };
        m.move_to(&w, target).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let after = m.rect(&w).unwrap();
        println!("before {before:?} after {after:?}");
        let _ = m.focus(&w);
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                hwnd(&w),
                windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                windows::Win32::Foundation::WPARAM(0),
                LPARAM(0),
            );
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&dir);
        // Outer rect includes the invisible resize border, so allow slack.
        assert!((after.x - target.x).abs() <= 16 && (after.y - target.y).abs() <= 16);
        assert!((after.w - target.w).abs() <= 16 && (after.h - target.h).abs() <= 16);
    }
}
