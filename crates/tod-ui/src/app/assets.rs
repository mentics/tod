//! The app's asset source: gpui-component's default icon bundle, plus the
//! icons from the same Lucide catalog (`gpui_kit_assets::IconName`) that the
//! bundle leaves out and the app uses.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

gpui_kit_assets::icon_assets!(
    ExtraIcons,
    [
        Pencil,
        CircleDot,
        MessagesSquare,
        Flag,
        FlagOff,
        Layers,
        ListChecks,
        Brain,
        Wrench
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // `ExtraIcons` answers `None` for anything it does not hold; the
        // default bundle errors on unknown paths, so it goes last.
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = ExtraIcons.list(path)?;
        paths.extend(gpui_kit_assets::Assets.list(path)?);
        Ok(paths)
    }
}

/// Whether the app's asset source has `path`.
#[cfg(test)]
pub fn serves(path: &str) -> bool {
    matches!(AppAssets.load(path), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::serves;

    #[test]
    fn title_bar_report_icon_is_bundled() {
        assert!(serves("icons/flag.svg"));
    }

    #[test]
    fn continue_in_terminal_icon_is_bundled() {
        use gpui_component::{IconName, IconNamed};
        assert!(serves(&IconName::SquareTerminal.path()));
    }
}
