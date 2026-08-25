use image::{DynamicImage, RgbaImage};
use std::path::Path;

/// Capture the app window, scale to logical `width`×`height`, optional crop, write PNG.
pub fn capture_window_png(
    path: &Path,
    logical_width: u32,
    logical_height: u32,
    crop: Option<(f32, f32, f32, f32)>,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        capture_windows(path, logical_width, logical_height, crop)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, logical_width, logical_height, crop);
        Err("screenshot unsupported on this platform".into())
    }
}

#[cfg(windows)]
fn capture_windows(
    path: &Path,
    logical_width: u32,
    logical_height: u32,
    crop: Option<(f32, f32, f32, f32)>,
) -> Result<(), String> {
    let rgba = capture_client_rgba()?;
    let scaled = DynamicImage::ImageRgba8(rgba)
        .resize_exact(
            logical_width,
            logical_height,
            image::imageops::FilterType::Triangle,
        )
        .into_rgba8();

    let out = if let Some((x0, y0, x1, y1)) = crop {
        let (x0, y0, x1, y1) = clamp_crop(x0, y0, x1, y1, logical_width, logical_height)?;
        DynamicImage::ImageRgba8(scaled)
            .crop_imm(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
            .into_rgba8()
    } else {
        scaled
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create_dir {}: {e}", parent.display()))?;
        }
    }
    out.save(path)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

fn clamp_crop(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    w: u32,
    h: u32,
) -> Result<(u32, u32, u32, u32), String> {
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
        return Err("crop coords must be finite".into());
    }
    if x1 <= x0 || y1 <= y0 {
        return Err("crop requires x1 > x0 and y1 > y0".into());
    }
    let x0 = x0.max(0.0).floor() as u32;
    let y0 = y0.max(0.0).floor() as u32;
    let x1 = x1.min(w as f32).ceil() as u32;
    let y1 = y1.min(h as f32).ceil() as u32;
    if x1 <= x0 || y1 <= y0 {
        return Err("crop rectangle is empty after clamp".into());
    }
    Ok((x0, y0, x1, y1))
}

#[cfg(windows)]
fn capture_client_rgba() -> Result<RgbaImage, String> {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

    let hwnd = find_main_hwnd().ok_or_else(|| "tod window not found".to_string())?;

    unsafe {
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_err() {
            return Err("GetClientRect failed".into());
        }
        let cw = (client.right - client.left).max(1);
        let ch = (client.bottom - client.top).max(1);

        let mut origin = POINT { x: 0, y: 0 };
        if !ClientToScreen(hwnd, &mut origin).as_bool() {
            return Err("ClientToScreen failed".into());
        }

        let hdc_screen = GetDC(HWND::default());
        if hdc_screen.is_invalid() {
            return Err("GetDC(screen) failed".into());
        }
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            ReleaseDC(HWND::default(), hdc_screen);
            return Err("CreateCompatibleDC failed".into());
        }
        let hbmp = CreateCompatibleBitmap(hdc_screen, cw, ch);
        if hbmp.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(HWND::default(), hdc_screen);
            return Err("CreateCompatibleBitmap failed".into());
        }
        let old = SelectObject(hdc_mem, HGDIOBJ(hbmp.0));
        if BitBlt(
            hdc_mem,
            0,
            0,
            cw,
            ch,
            hdc_screen,
            origin.x,
            origin.y,
            SRCCOPY,
        )
        .is_err()
        {
            SelectObject(hdc_mem, old);
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(HWND::default(), hdc_screen);
            return Err("BitBlt failed".into());
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
        ReleaseDC(HWND::default(), hdc_screen);

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
fn find_main_hwnd() -> Option<windows::Win32::Foundation::HWND> {
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

/// Post a left-button click at logical client coords (no OS focus required).
pub fn post_click(
    logical_x: f32,
    logical_y: f32,
    logical_width: u32,
    logical_height: u32,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        post_click_windows(logical_x, logical_y, logical_width, logical_height)
    }
    #[cfg(not(windows))]
    {
        let _ = (logical_x, logical_y, logical_width, logical_height);
        Err("click unsupported on this platform".into())
    }
}

#[cfg(windows)]
fn post_click_windows(
    logical_x: f32,
    logical_y: f32,
    logical_width: u32,
    logical_height: u32,
) -> Result<(), String> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, PostMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    };

    let hwnd = find_main_hwnd().ok_or_else(|| "tod window not found".to_string())?;
    let (cw, ch) = unsafe {
        let mut client = windows::Win32::Foundation::RECT::default();
        GetClientRect(hwnd, &mut client).map_err(|e| format!("GetClientRect: {e}"))?;
        (
            (client.right - client.left).max(1) as f32,
            (client.bottom - client.top).max(1) as f32,
        )
    };
    let scale_x = cw / logical_width.max(1) as f32;
    let scale_y = ch / logical_height.max(1) as f32;
    let x = (logical_x * scale_x).round() as i32;
    let y = (logical_y * scale_y).round() as i32;
    let lp = mouse_lparam(x, y);
    const MK_LBUTTON: usize = 0x0001;

    unsafe {
        PostMessageW(hwnd, WM_MOUSEMOVE, WPARAM(0), lp)
            .map_err(|e| format!("PostMessage MOVE: {e}"))?;
        PostMessageW(hwnd, WM_LBUTTONDOWN, WPARAM(MK_LBUTTON), lp)
            .map_err(|e| format!("PostMessage DOWN: {e}"))?;
        PostMessageW(hwnd, WM_LBUTTONUP, WPARAM(0), lp)
            .map_err(|e| format!("PostMessage UP: {e}"))?;
    }
    Ok(())
}

#[cfg(windows)]
fn mouse_lparam(x: i32, y: i32) -> windows::Win32::Foundation::LPARAM {
    use windows::Win32::Foundation::LPARAM;
    let packed = ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF);
    LPARAM(packed as isize)
}
