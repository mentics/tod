use crate::ui::screenshot::write_png_fast;
use image::DynamicImage;
#[cfg(windows)]
use image::RgbaImage;
use std::path::Path;

#[cfg(windows)]
use crate::ui::screenshot::{capture_client_rgba, main_hwnd};

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

/// Left-button click at logical client coords via `SendMessage` (no OS focus, waits for handling).
pub fn send_click(
    logical_x: f32,
    logical_y: f32,
    logical_width: u32,
    logical_height: u32,
    right: bool,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        send_click_windows(logical_x, logical_y, logical_width, logical_height, right)
    }
    #[cfg(not(windows))]
    {
        let _ = (logical_x, logical_y, logical_width, logical_height, right);
        Err("click unsupported on this platform".into())
    }
}

#[cfg(windows)]
fn send_click_windows(
    logical_x: f32,
    logical_y: f32,
    logical_width: u32,
    logical_height: u32,
    right: bool,
) -> Result<(), String> {
    use windows::Win32::Foundation::WPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, SendMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN,
        WM_RBUTTONUP,
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
    const MK_RBUTTON: usize = 0x0002;
    let (down, up, held) = if right {
        (WM_RBUTTONDOWN, WM_RBUTTONUP, MK_RBUTTON)
    } else {
        (WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON)
    };

    unsafe {
        // SendMessage blocks until the window proc handles the message — no sleep needed.
        SendMessageW(hwnd, WM_MOUSEMOVE, WPARAM(0), lp);
        SendMessageW(hwnd, down, WPARAM(held), lp);
        SendMessageW(hwnd, up, WPARAM(0), lp);
    }
    Ok(())
}

/// One step of a drag held open: press, move, or release.
///
/// `drag` does the whole gesture in one message and is what a test wants. This
/// is for looking at the UI *during* a drag — the landing line a list draws
/// under the pointer only exists while the button is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragStep {
    Press,
    Move,
    Release,
}

pub fn send_drag_step(
    step: DragStep,
    logical_x: f32,
    logical_y: f32,
    logical_width: u32,
    logical_height: u32,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        send_drag_step_windows(step, logical_x, logical_y, logical_width, logical_height)
    }
    #[cfg(not(windows))]
    {
        let _ = (step, logical_x, logical_y, logical_width, logical_height);
        Err("drag steps unsupported on this platform".into())
    }
}

#[cfg(windows)]
fn send_drag_step_windows(
    step: DragStep,
    logical_x: f32,
    logical_y: f32,
    logical_width: u32,
    logical_height: u32,
) -> Result<(), String> {
    use windows::Win32::Foundation::WPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, SendMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    };

    const MK_LBUTTON: usize = 0x0001;
    let hwnd = main_hwnd().ok_or_else(|| "tod window not found".to_string())?;
    let (cw, ch) = unsafe {
        let mut client = windows::Win32::Foundation::RECT::default();
        GetClientRect(hwnd, &mut client).map_err(|e| format!("GetClientRect: {e}"))?;
        (
            (client.right - client.left).max(1) as f32,
            (client.bottom - client.top).max(1) as f32,
        )
    };
    let x = (logical_x * cw / logical_width.max(1) as f32).round() as i32;
    let y = (logical_y * ch / logical_height.max(1) as f32).round() as i32;
    let lp = mouse_lparam(x, y);
    let (message, held) = match step {
        DragStep::Press => (WM_LBUTTONDOWN, MK_LBUTTON),
        DragStep::Move => (WM_MOUSEMOVE, MK_LBUTTON),
        DragStep::Release => (WM_LBUTTONUP, 0),
    };
    unsafe {
        if step == DragStep::Press {
            SendMessageW(hwnd, WM_MOUSEMOVE, WPARAM(0), lp);
        }
        SendMessageW(hwnd, message, WPARAM(held), lp);
    }
    Ok(())
}

/// Press at one point, move to another in steps, and release: a drag.
///
/// The steps matter. A drag is not a click with a different end point — the UI
/// starts one only after the pointer has moved while held, and a drop target
/// only lights up once the pointer has been reported over it.
pub fn send_drag(
    from: (f32, f32),
    to: (f32, f32),
    logical_width: u32,
    logical_height: u32,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        send_drag_windows(from, to, logical_width, logical_height)
    }
    #[cfg(not(windows))]
    {
        let _ = (from, to, logical_width, logical_height);
        Err("drag unsupported on this platform".into())
    }
}

#[cfg(windows)]
fn send_drag_windows(
    from: (f32, f32),
    to: (f32, f32),
    logical_width: u32,
    logical_height: u32,
) -> Result<(), String> {
    use windows::Win32::Foundation::WPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, SendMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    };

    /// Enough intermediate moves for the drag to start and for the target
    /// under the pointer to be seen before the button comes up.
    const STEPS: i32 = 12;
    const MK_LBUTTON: usize = 0x0001;

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
    let at = |x: f32, y: f32| mouse_lparam((x * scale_x).round() as i32, (y * scale_y).round() as i32);

    unsafe {
        // SendMessage blocks until the window proc has handled each message,
        // so the frames in between happen without any sleeping here.
        SendMessageW(hwnd, WM_MOUSEMOVE, WPARAM(0), at(from.0, from.1));
        SendMessageW(hwnd, WM_LBUTTONDOWN, WPARAM(MK_LBUTTON), at(from.0, from.1));
        for step in 1..=STEPS {
            let progress = step as f32 / STEPS as f32;
            let x = from.0 + (to.0 - from.0) * progress;
            let y = from.1 + (to.1 - from.1) * progress;
            SendMessageW(hwnd, WM_MOUSEMOVE, WPARAM(MK_LBUTTON), at(x, y));
        }
        SendMessageW(hwnd, WM_LBUTTONUP, WPARAM(0), at(to.0, to.1));
    }
    Ok(())
}

#[cfg(windows)]
fn mouse_lparam(x: i32, y: i32) -> windows::Win32::Foundation::LPARAM {
    use windows::Win32::Foundation::LPARAM;
    let packed = ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF);
    LPARAM(packed as isize)
}
