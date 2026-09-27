//! App-window screenshot capture, shared by the agent control socket and the
//! "report a problem" feature (`crate::ui::report_problem`).
//!
//! Capture must never run on the GPUI UI thread: callers spawn it on a
//! background OS thread (the agent socket already does this per-connection;
//! `report_problem` does it via the background executor) and hand the result
//! back through a channel/task.

use image::RgbaImage;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Mutex;

/// Capture the app's main window as RGBA pixels, at native (physical) size.
///
/// Returns `None` when there is no window to capture, or on platforms this
/// isn't implemented for yet (anything but Windows, currently).
pub fn capture_app_screenshot() -> Option<RgbaImage> {
    #[cfg(windows)]
    {
        capture_client_rgba().ok()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Encode an RGBA image as a PNG to `path`, creating parent directories as
/// needed. Used both by the agent socket's `shot` command and by
/// `report_problem`'s bundle attachment.
pub fn write_png_fast(path: &Path, img: &RgbaImage) -> Result<(), String> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create_dir {}: {e}", parent.display()))?;
        }
    }
    let file = File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let encoder =
        PngEncoder::new_with_quality(&mut writer, CompressionType::Fast, FilterType::Adaptive);
    encoder
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Encode an RGBA image straight to an in-memory PNG buffer.
pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};

    let mut buf = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut buf, CompressionType::Fast, FilterType::Adaptive);
    encoder
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("encode png: {e}"))?;
    Ok(buf)
}

#[cfg(windows)]
pub(crate) fn capture_client_rgba() -> Result<RgbaImage, String> {
    use windows::Win32::Foundation::{BOOL, HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HGDIOBJ, ReleaseDC, SelectObject,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn PrintWindow(hwnd: HWND, hdc_blt: HDC, flags: u32) -> BOOL;
    }
    use windows::Win32::Graphics::Gdi::HDC;

    // Prefer PW_RENDERFULLCONTENT so GPU/DWM content is included even when covered.
    const PW_RENDERFULLCONTENT: u32 = 0x00000002;
    const PW_CLIENTONLY: u32 = 0x00000001;

    let hwnd = main_hwnd().ok_or_else(|| "tod window not found".to_string())?;

    unsafe {
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_err() {
            return Err("GetClientRect failed".into());
        }
        let cw = (client.right - client.left).max(1);
        let ch = (client.bottom - client.top).max(1);

        let hdc_window = GetDC(hwnd);
        if hdc_window.is_invalid() {
            return Err("GetDC(hwnd) failed".into());
        }
        let hdc_mem = CreateCompatibleDC(hdc_window);
        if hdc_mem.is_invalid() {
            ReleaseDC(hwnd, hdc_window);
            return Err("CreateCompatibleDC failed".into());
        }
        let hbmp = CreateCompatibleBitmap(hdc_window, cw, ch);
        if hbmp.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_window);
            return Err("CreateCompatibleBitmap failed".into());
        }
        let old = SelectObject(hdc_mem, HGDIOBJ(hbmp.0));

        let printed = PrintWindow(hwnd, hdc_mem, PW_CLIENTONLY | PW_RENDERFULLCONTENT).as_bool()
            || PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT).as_bool()
            || PrintWindow(hwnd, hdc_mem, 0).as_bool();

        if !printed {
            SelectObject(hdc_mem, old);
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_window);
            return Err("PrintWindow failed".into());
        }

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: cw,
                biHeight: -ch, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (cw * ch * 4) as usize];
        let got = GetDIBits(
            hdc_mem,
            hbmp,
            0,
            ch as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(hdc_mem, old);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_window);

        if got == 0 {
            return Err("GetDIBits failed".into());
        }

        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        RgbaImage::from_raw(cw as u32, ch as u32, buf)
            .ok_or_else(|| "invalid bitmap buffer".to_string())
    }
}

#[cfg(windows)]
static HWND_CACHE: Mutex<Option<isize>> = Mutex::new(None);

#[cfg(windows)]
pub(crate) fn main_hwnd() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    if let Ok(guard) = HWND_CACHE.lock() {
        if let Some(raw) = *guard {
            let hwnd = HWND(raw);
            if unsafe { IsWindow(hwnd).as_bool() } {
                return Some(hwnd);
            }
        }
    }
    let found = find_main_hwnd()?;
    if let Ok(mut guard) = HWND_CACHE.lock() {
        *guard = Some(found.0);
    }
    Some(found)
}

#[cfg(windows)]
pub(crate) fn find_main_hwnd() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
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
