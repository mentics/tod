use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{DynamicImage, ExtendedColorType, ImageEncoder, RgbaImage};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Mutex;

/// Capture the app window, scale to logical `width`×`height`, optional crop, write PNG.
///
/// Lean path: crop in physical pixels first when a crop is given, skip resize when sizes
/// already match, and encode PNG with fast compression.
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
    let (pw, ph) = (rgba.width(), rgba.height());
    let lw = logical_width.max(1);
    let lh = logical_height.max(1);
    let scale_x = pw as f32 / lw as f32;
    let scale_y = ph as f32 / lh as f32;

    let out = if let Some((x0, y0, x1, y1)) = crop {
        let (lx0, ly0, lx1, ly1) = clamp_crop(x0, y0, x1, y1, lw, lh)?;
        let tw = lx1.saturating_sub(lx0).max(1);
        let th = ly1.saturating_sub(ly0).max(1);

        // Crop in physical space first (fewer pixels to resize/encode).
        let px0 = ((lx0 as f32) * scale_x).floor().max(0.0) as u32;
        let py0 = ((ly0 as f32) * scale_y).floor().max(0.0) as u32;
        let px1 = ((lx1 as f32) * scale_x).ceil().min(pw as f32) as u32;
        let py1 = ((ly1 as f32) * scale_y).ceil().min(ph as f32) as u32;
        if px1 <= px0 || py1 <= py0 {
            return Err("crop rectangle is empty after physical clamp".into());
        }
        let cropped = DynamicImage::ImageRgba8(rgba).crop_imm(px0, py0, px1 - px0, py1 - py0);
        if cropped.width() == tw && cropped.height() == th {
            cropped.into_rgba8()
        } else {
            cropped
                .resize_exact(tw, th, image::imageops::FilterType::Triangle)
                .into_rgba8()
        }
    } else if pw == lw && ph == lh {
        rgba
    } else {
        DynamicImage::ImageRgba8(rgba)
            .resize_exact(lw, lh, image::imageops::FilterType::Triangle)
            .into_rgba8()
    };

    write_png_fast(path, &out)
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

fn write_png_fast(path: &Path, img: &RgbaImage) -> Result<(), String> {
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

#[cfg(windows)]
fn capture_client_rgba() -> Result<RgbaImage, String> {
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
fn main_hwnd() -> Option<windows::Win32::Foundation::HWND> {
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

/// Left-button click at logical client coords via `SendMessage` (no OS focus, waits for handling).
pub fn send_click(
    logical_x: f32,
    logical_y: f32,
    logical_width: u32,
    logical_height: u32,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        send_click_windows(logical_x, logical_y, logical_width, logical_height)
    }
    #[cfg(not(windows))]
    {
        let _ = (logical_x, logical_y, logical_width, logical_height);
        Err("click unsupported on this platform".into())
    }
}

#[cfg(windows)]
fn send_click_windows(
    logical_x: f32,
    logical_y: f32,
    logical_width: u32,
    logical_height: u32,
) -> Result<(), String> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, SendMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    };

    let hwnd = main_hwnd().ok_or_else(|| "tod window not found".to_string())?;
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
        // SendMessage blocks until the window proc handles the message — no sleep needed.
        SendMessageW(hwnd, WM_MOUSEMOVE, WPARAM(0), lp);
        SendMessageW(hwnd, WM_LBUTTONDOWN, WPARAM(MK_LBUTTON), lp);
        SendMessageW(hwnd, WM_LBUTTONUP, WPARAM(0), lp);
    }
    Ok(())
}

#[cfg(windows)]
fn mouse_lparam(x: i32, y: i32) -> windows::Win32::Foundation::LPARAM {
    use windows::Win32::Foundation::LPARAM;
    let packed = ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF);
    LPARAM(packed as isize)
}
