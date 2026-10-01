//! The `Browser` trait and its registry (design section 5).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::chrome::Chrome;
use super::placement::NativeRect;

/// A window rectangle in the browser's own (native pixel) units.
pub type BrowserRect = NativeRect;

/// What a browser can do. Drives which UI is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// A chrome-less standalone window.
    pub app_mode: bool,
    /// Honours position/size flags at launch.
    pub launch_placement: bool,
    /// `--user-data-dir` or equivalent.
    pub separate_profile: bool,
}

/// Why no browser could be used. There is no fallback browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserError {
    NotFound { browser: &'static str },
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserError::NotFound { browser } => write!(
                f,
                "{browser} was not found. Install it, or set visual_design.browser to its path."
            ),
        }
    }
}

impl std::error::Error for BrowserError {}

pub trait Browser: Send + Sync {
    /// Stable id for settings and logs, e.g. "chrome".
    fn id(&self) -> &'static str;
    /// Where it is installed on this machine, or None.
    fn discover(&self) -> Option<PathBuf>;
    fn capabilities(&self) -> Capabilities;
    /// Extra flags that let tod read the page back (screenshots): remote
    /// debugging on a port the browser picks, written to its profile.
    fn debug_args(&self) -> Vec<OsString> {
        Vec::new()
    }
    /// The command line that opens `url` as a standalone window using
    /// `profile`. `placement` is Some only on a first launch.
    fn launch_args(
        &self,
        url: &str,
        profile: &Path,
        placement: Option<BrowserRect>,
    ) -> Vec<OsString>;
}

/// The browsers in preference order.
pub fn registry() -> Vec<Box<dyn Browser>> {
    vec![Box::new(Chrome::new(None))]
}

/// The registry with the `visual_design.browser` path override applied.
pub fn registry_with_override(path: Option<PathBuf>) -> Vec<Box<dyn Browser>> {
    vec![Box::new(Chrome::new(path))]
}

/// First browser that is installed, with its executable.
pub fn first_available(
    browsers: &[Box<dyn Browser>],
) -> Result<(&dyn Browser, PathBuf), BrowserError> {
    for b in browsers {
        if let Some(p) = b.discover() {
            return Ok((b.as_ref(), p));
        }
    }
    Err(BrowserError::NotFound {
        browser: browsers.first().map(|b| b.id()).unwrap_or("a browser"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_prefers_chrome() {
        assert_eq!(registry()[0].id(), "chrome");
    }

    #[test]
    fn missing_browser_is_an_error_value() {
        let b: Vec<Box<dyn Browser>> = vec![Box::new(Chrome::with_probe(
            None,
            Box::new(super::super::chrome::NoProbe),
        ))];
        assert!(matches!(
            first_available(&b),
            Err(BrowserError::NotFound { browser: "chrome" })
        ));
    }

    #[test]
    fn an_override_path_is_honoured_and_a_bogus_one_is_not_found() {
        let exe = std::env::current_exe().unwrap();
        let ok = registry_with_override(Some(exe.clone()));
        assert_eq!(first_available(&ok).map(|(_, p)| p), Ok(exe));
        let bogus = registry_with_override(Some("/definitely/not/a/browser".into()));
        assert!(matches!(
            first_available(&bogus),
            Err(BrowserError::NotFound { browser: "chrome" })
        ));
    }
}
