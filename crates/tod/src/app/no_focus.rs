//! Keep startup from stealing OS focus (e2e / agent control socket).

#[cfg(windows)]
use windows::Win32::Foundation::HWND;

/// Foreground window before opening ours (Windows only).
#[cfg(windows)]
pub fn foreground_hwnd() -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 == 0 { None } else { Some(hwnd) }
}

/// After the main window is created, show it without activating (Windows only).
/// macOS/Linux honor `WindowOptions::focus = false` at creation time.
#[cfg(windows)]
pub fn after_window_open(previous_foreground: Option<HWND>) {
    use windows::Win32::UI::WindowsAndMessaging::{
        IsWindow, SW_SHOWNOACTIVATE, SetForegroundWindow, ShowWindow,
    };

    let Some(hwnd) = main_hwnd() else {
        return;
    };

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if let Some(prev) = previous_foreground {
            if prev != hwnd && IsWindow(prev).as_bool() {
                let _ = SetForegroundWindow(prev);
            }
        }
    }
}

#[cfg(windows)]
pub(crate) fn main_hwnd() -> Option<HWND> {
    use windows::Win32::Foundation::{BOOL, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClientRect, GetWindowThreadProcessId, IsWindowVisible,
    };

    struct State {
        pid: u32,
        best: Option<(HWND, i32)>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut State) };
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut wpid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut wpid));
            if wpid != state.pid {
                return BOOL(1);
            }
            let mut rect = windows::Win32::Foundation::RECT::default();
            if GetClientRect(hwnd, &mut rect).is_err() {
                return BOOL(1);
            }
            let area = (rect.right - rect.left).saturating_mul(rect.bottom - rect.top);
            if area <= 0 {
                return BOOL(1);
            }
            match state.best {
                Some((_, best_area)) if best_area >= area => {}
                _ => state.best = Some((hwnd, area)),
            }
            BOOL(1)
        }
    }

    let pid = std::process::id();
    let mut state = State { pid, best: None };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }
    state.best.map(|(h, _)| h)
}
