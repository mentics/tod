//! gpui-specific helpers for persisted settings (core types live in `tod-store`).

pub use tod_store::settings::{
    AnswerProcessorSettings, MAX_LOG_MAX_SIZE_KB, MIN_LOG_MAX_SIZE_KB, QuestionMakerSettings,
    TodSettings, WindowGeometry, WorktreeBackend,
};

use crate::interview::paths::TodPaths;
use gpui::{Bounds, Pixels, Window, WindowBounds, point, px, size};

fn window_geometry_to_bounds(geometry: &WindowGeometry) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(geometry.x), px(geometry.y)),
        size: size(px(geometry.width), px(geometry.height)),
    }
}

pub fn window_geometry_to_window_bounds(geometry: &WindowGeometry) -> WindowBounds {
    let bounds = window_geometry_to_bounds(geometry);
    if geometry.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

pub fn geometry_from_window(window: &Window) -> WindowGeometry {
    match window.window_bounds() {
        WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds) => {
            let maximized = matches!(window.window_bounds(), WindowBounds::Maximized(_));
            WindowGeometry {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
                maximized,
            }
        }
        WindowBounds::Fullscreen(bounds) => WindowGeometry {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
            maximized: false,
        },
    }
}

pub fn persist_window_geometry(window: &Window, paths: &TodPaths) {
    let geometry = geometry_from_window(window);
    match TodSettings::load(paths) {
        Ok(mut settings) => {
            settings.window_geometry = Some(geometry);
            if let Err(err) = settings.save(paths) {
                tracing::error!("failed to save window geometry: {err:#}");
            }
        }
        Err(err) => {
            tracing::error!("failed to load settings for window geometry save: {err:#}");
        }
    }
}

pub fn resolve_open_window_bounds(
    settings: &TodSettings,
    default_width: f32,
    default_height: f32,
    width_from_cli: bool,
    height_from_cli: bool,
) -> WindowBounds {
    let Some(mut geometry) = settings.window_geometry.clone() else {
        return WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(default_width), px(default_height)),
        });
    };
    if width_from_cli {
        geometry.width = default_width;
    }
    if height_from_cli {
        geometry.height = default_height;
    }
    window_geometry_to_window_bounds(&geometry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::paths::{clear_data_root_override, set_data_root};
    use std::fs;

    #[test]
    fn resolve_open_window_bounds_uses_saved_geometry() {
        let settings = TodSettings {
            window_geometry: Some(WindowGeometry {
                x: 100.0,
                y: 200.0,
                width: 1600.0,
                height: 900.0,
                maximized: false,
            }),
            ..TodSettings::default()
        };
        let bounds = resolve_open_window_bounds(&settings, 1280.0, 768.0, false, false);
        let WindowBounds::Windowed(saved) = bounds else {
            panic!("expected windowed bounds");
        };
        assert_eq!(f32::from(saved.origin.x), 100.0);
        assert_eq!(f32::from(saved.origin.y), 200.0);
        assert_eq!(f32::from(saved.size.width), 1600.0);
        assert_eq!(f32::from(saved.size.height), 900.0);
    }

    #[test]
    fn resolve_open_window_bounds_honors_cli_size_overrides() {
        let settings = TodSettings {
            window_geometry: Some(WindowGeometry {
                x: 100.0,
                y: 200.0,
                width: 1600.0,
                height: 900.0,
                maximized: false,
            }),
            ..TodSettings::default()
        };
        let bounds = resolve_open_window_bounds(&settings, 1024.0, 600.0, true, true);
        let WindowBounds::Windowed(saved) = bounds else {
            panic!("expected windowed bounds");
        };
        assert_eq!(f32::from(saved.origin.x), 100.0);
        assert_eq!(f32::from(saved.origin.y), 200.0);
        assert_eq!(f32::from(saved.size.width), 1024.0);
        assert_eq!(f32::from(saved.size.height), 600.0);
    }

    #[test]
    fn resolve_fleet_storage_root_uses_data_root() {
        let sandbox =
            std::env::temp_dir().join(format!("tod-fleet-sandbox-{}", uuid::Uuid::new_v4()));
        set_data_root(sandbox.clone());
        let paths = TodPaths::discover().unwrap();
        let settings = TodSettings::default();
        let root = settings.resolve_fleet_storage_root(&paths).unwrap();
        assert_eq!(root, sandbox);
        clear_data_root_override();
        let _ = fs::remove_dir_all(sandbox);
    }
}
