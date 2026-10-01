//! macOS mover: `osascript` through System Events (needs Accessibility).

use super::{MoverError, WindowHandle, WindowMover};
use crate::visual_design::placement::NativeRect;
use std::process::Command;

pub struct MacMover;

const GRANT: &str =
    "grant Accessibility access in System Settings > Privacy & Security > Accessibility";

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn run(script: &str) -> Result<String, MoverError> {
    let out = Command::new("osascript")
        .args(["-e", script])
        .output()
        .map_err(|e| MoverError::Unsupported(format!("osascript: {e}")))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if err.contains("-1719") || err.contains("-25211") || err.contains("not allowed assistive") {
        Err(MoverError::PermissionDenied(GRANT.into()))
    } else {
        Err(MoverError::Failed(err.trim().to_string()))
    }
}

impl WindowMover for MacMover {
    fn find(&self, title_prefix: &str) -> Option<WindowHandle> {
        let script = format!(
            "tell application \"System Events\"\nrepeat with p in (processes whose visible is true)\nrepeat with w in windows of p\nif name of w starts with \"{}\" then return name of w\nend repeat\nend repeat\nreturn \"\"\nend tell",
            escape(title_prefix)
        );
        let title = run(&script).ok()?;
        (!title.is_empty()).then_some(WindowHandle { id: 0, title })
    }

    fn move_to(&self, w: &WindowHandle, r: NativeRect) -> Result<(), MoverError> {
        let t = escape(&w.title);
        run(&format!(
            "tell application \"System Events\"\nrepeat with p in (processes whose visible is true)\nif exists (window \"{t}\" of p) then\nset position of window \"{t}\" of p to {{{}, {}}}\nset size of window \"{t}\" of p to {{{}, {}}}\nend if\nend repeat\nend tell",
            r.x, r.y, r.w, r.h
        ))
        .map(|_| ())
    }

    fn focus(&self, w: &WindowHandle) -> Result<(), MoverError> {
        let t = escape(&w.title);
        run(&format!(
            "tell application \"System Events\"\nrepeat with p in (processes whose visible is true)\nif exists (window \"{t}\" of p) then\nset frontmost of p to true\nperform action \"AXRaise\" of window \"{t}\" of p\nend if\nend repeat\nend tell"
        ))
        .map(|_| ())
    }
}
