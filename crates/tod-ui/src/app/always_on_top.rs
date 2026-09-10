//! Toggle whether the main window stays above other windows.

#[cfg(windows)]
use super::no_focus::main_hwnd;

/// Whether always-on-top can be toggled on this platform.
pub fn is_supported() -> bool {
    cfg!(windows)
}

/// Apply always-on-top to the main application window.
///
/// Returns `true` when the platform accepted the change.
pub fn set(enabled: bool) -> bool {
    #[cfg(windows)]
    {
        return set_windows(enabled);
    }

    #[cfg(not(windows))]
    {
        let _ = enabled;
        false
    }
}

#[cfg(windows)]
fn set_windows(enabled: bool) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };

    let Some(hwnd) = main_hwnd() else {
        return false;
    };

    let insert_after = if enabled {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };

    unsafe {
        SetWindowPos(
            hwnd,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
        .is_ok()
    }
}
