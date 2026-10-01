//! Where the design window goes (design section 6.3). Pure maths, no GPUI.
//!
//! Units: all `Rect`s are **logical** pixels (GPUI bounds and display work
//! areas divided by the window's scale factor). Chrome's `--window-position`
//! and `--window-size` also take logical pixels, so pass `dock_rect`'s result
//! to them unchanged. `SetWindowPos` on Windows takes **physical** pixels, so
//! convert once with [`to_native_units`] for the mover.

/// A rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    fn contains(&self, p: (f64, f64)) -> bool {
        p.0 >= self.x && p.0 < self.right() && p.1 >= self.y && p.1 < self.bottom()
    }
}

/// A rectangle in the units the OS window API takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }
}

pub const DEFAULT_MIN_WIDTH: f64 = 700.0;
pub const MIN_SIZE: (f64, f64) = (480.0, 360.0);

/// The work area that contains the centre of `tod`; falls back to the first.
pub fn display_for(tod: Rect, work_areas: &[Rect]) -> Option<Rect> {
    let c = tod.center();
    work_areas
        .iter()
        .find(|a| a.contains(c))
        .or_else(|| work_areas.first())
        .copied()
}

/// Where the browser window goes beside `tod`, in logical pixels.
///
/// Right strip if at least `min_width` is free, else the left strip, else the
/// right 45% of the work area (tod is never moved). Clamped to the work area,
/// never smaller than 480x360 (unless the work area is).
pub fn dock_rect(tod: Rect, work_area: Rect, min_width: f64) -> Rect {
    let right_free = work_area.right() - tod.right();
    let left_free = tod.x - work_area.x;
    let top = tod.y.clamp(work_area.y, work_area.bottom());
    let r = if right_free >= min_width {
        Rect::new(tod.right(), top, right_free, work_area.bottom() - top)
    } else if left_free >= min_width {
        Rect::new(work_area.x, top, left_free, work_area.bottom() - top)
    } else {
        let w = work_area.w * 0.45;
        Rect::new(work_area.right() - w, work_area.y, w, work_area.h)
    };
    clamp(r, work_area)
}

fn clamp(r: Rect, area: Rect) -> Rect {
    let w = r.w.max(MIN_SIZE.0).min(area.w);
    let h = r.h.max(MIN_SIZE.1).min(area.h);
    let x = r.x.max(area.x).min(area.right() - w);
    let y = r.y.max(area.y).min(area.bottom() - h);
    Rect::new(x, y, w, h)
}

/// Converts a logical rect to the units the OS mover takes.
///
/// Windows and Linux (X11) window APIs take physical pixels, so multiply by
/// `scale_factor` (1.5 at 150%). macOS accessibility positions are in points,
/// which are already logical, so the rect is only rounded. Do this once, for
/// the mover; Chrome's launch flags take the logical rect unchanged.
pub fn to_native_units(rect: Rect, scale_factor: f64, platform: Platform) -> NativeRect {
    let s = match platform {
        Platform::Windows | Platform::Linux => scale_factor,
        Platform::MacOs => 1.0,
    };
    NativeRect {
        x: (rect.x * s).round() as i32,
        y: (rect.y * s).round() as i32,
        w: (rect.w * s).round() as i32,
        h: (rect.h * s).round() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1040.0 };

    #[test]
    fn right_strip() {
        let tod = Rect::new(0.0, 100.0, 1000.0, 800.0);
        let r = dock_rect(tod, WORK, 700.0);
        assert_eq!(r, Rect::new(1000.0, 100.0, 920.0, 940.0));
    }

    #[test]
    fn left_strip_when_right_too_small() {
        let tod = Rect::new(900.0, 0.0, 1000.0, 800.0);
        let r = dock_rect(tod, WORK, 700.0);
        assert_eq!(r, Rect::new(0.0, 0.0, 900.0, 1040.0));
    }

    #[test]
    fn tod_fills_display_overlays_right_45_percent() {
        let r = dock_rect(WORK, WORK, 700.0);
        assert_eq!(r, Rect::new(1056.0, 0.0, 864.0, 1040.0));
    }

    #[test]
    fn multi_display_uses_center_display() {
        let a = Rect::new(0.0, 0.0, 1920.0, 1040.0);
        let b = Rect::new(1920.0, 0.0, 1920.0, 1040.0);
        let tod = Rect::new(1800.0, 0.0, 600.0, 800.0); // centre x = 2100, on b
        assert_eq!(display_for(tod, &[a, b]), Some(b));
        let r = dock_rect(tod, b, 700.0);
        assert!(r.x >= b.x && r.right() <= b.right());
    }

    #[test]
    fn clamps_to_work_area_and_minimum_size() {
        let tod = Rect::new(0.0, 900.0, 1000.0, 140.0);
        let r = dock_rect(tod, WORK, 700.0);
        assert!(r.bottom() <= WORK.bottom());
        assert!(r.h >= MIN_SIZE.1);
        assert!(r.w >= MIN_SIZE.0);
    }

    #[test]
    fn native_units_scale() {
        let r = Rect::new(1000.0, 100.0, 900.0, 700.0);
        for (scale, w) in [(1.0, 900), (1.5, 1350), (2.0, 1800)] {
            let n = to_native_units(r, scale, Platform::Windows);
            assert_eq!(n.w, w);
            assert_eq!(n.x, (1000.0 * scale) as i32);
            assert_eq!(n.h, (700.0 * scale) as i32);
        }
        let n = to_native_units(r, 2.0, Platform::MacOs);
        assert_eq!(n, NativeRect { x: 1000, y: 100, w: 900, h: 700 });
    }
}
