//! Linux mover: X11 through `wmctrl` (else `xdotool`). Wayland, or neither
//! tool installed, is `Unsupported`.

use super::{MoverError, WindowHandle, WindowMover};
use crate::visual_design::placement::NativeRect;
use std::process::Command;

pub struct LinuxMover;

fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("DISPLAY").is_none()
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

fn unsupported() -> Option<MoverError> {
    if wayland() {
        return Some(MoverError::Unsupported("Wayland does not let apps move other windows".into()));
    }
    if !have("wmctrl") && !have("xdotool") {
        return Some(MoverError::Unsupported("install wmctrl or xdotool".into()));
    }
    None
}

fn run(cmd: &mut Command) -> Result<String, MoverError> {
    let out = cmd.output().map_err(|e| MoverError::Failed(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(MoverError::Failed(String::from_utf8_lossy(&out.stderr).trim().to_string()))
    }
}

/// Parse `wmctrl -l` output: `0x04200003  0 host Title...`.
fn parse_wmctrl_list(out: &str, prefix: &str) -> Option<WindowHandle> {
    out.lines().find_map(|line| {
        let id = line.split_whitespace().next()?;
        // Title is everything after the third whitespace-separated field.
        let mut rest = line.trim_start();
        for _ in 0..3 {
            rest = rest.trim_start_matches(|c: char| !c.is_whitespace()).trim_start();
        }
        let title = rest.trim().to_string();
        title.starts_with(prefix).then(|| WindowHandle {
            id: u64::from_str_radix(id.trim_start_matches("0x"), 16).unwrap_or(0),
            title,
        })
    })
}

impl WindowMover for LinuxMover {
    fn find(&self, title_prefix: &str) -> Option<WindowHandle> {
        if unsupported().is_some() {
            return None;
        }
        if have("wmctrl") {
            let out = run(Command::new("wmctrl").arg("-l")).ok()?;
            return parse_wmctrl_list(&out, title_prefix);
        }
        let out = run(Command::new("xdotool").args(["search", "--name", &format!("^{title_prefix}")])).ok()?;
        let id: u64 = out.lines().next()?.trim().parse().ok()?;
        let title = run(Command::new("xdotool").args(["getwindowname", &id.to_string()])).ok()?;
        Some(WindowHandle { id, title: title.trim().to_string() })
    }

    fn move_to(&self, w: &WindowHandle, r: NativeRect) -> Result<(), MoverError> {
        if let Some(e) = unsupported() {
            return Err(e);
        }
        if have("wmctrl") {
            let id = format!("0x{:x}", w.id);
            run(Command::new("wmctrl").args(["-i", "-r", &id, "-b", "remove,maximized_vert,maximized_horz"]))?;
            run(Command::new("wmctrl").args([
                "-i", "-r", &id, "-e",
                &format!("0,{},{},{},{}", r.x, r.y, r.w, r.h),
            ]))
            .map(|_| ())
        } else {
            let id = w.id.to_string();
            run(Command::new("xdotool").args(["windowsize", &id, &r.w.to_string(), &r.h.to_string()]))?;
            run(Command::new("xdotool").args(["windowmove", &id, &r.x.to_string(), &r.y.to_string()]))
                .map(|_| ())
        }
    }

    fn hide(&self, w: &WindowHandle) -> Result<(), MoverError> {
        if let Some(e) = unsupported() {
            return Err(e);
        }
        if have("xdotool") {
            return run(Command::new("xdotool").args(["windowunmap", &w.id.to_string()])).map(|_| ());
        }
        run(Command::new("wmctrl").args(["-i", "-r", &format!("0x{:x}", w.id), "-b", "add,hidden"])).map(|_| ())
    }

    fn show(&self, w: &WindowHandle) -> Result<(), MoverError> {
        if let Some(e) = unsupported() {
            return Err(e);
        }
        if have("xdotool") {
            return run(Command::new("xdotool").args(["windowmap", &w.id.to_string()])).map(|_| ());
        }
        run(Command::new("wmctrl").args(["-i", "-r", &format!("0x{:x}", w.id), "-b", "remove,hidden"])).map(|_| ())
    }

    fn close(&self, w: &WindowHandle) -> Result<(), MoverError> {
        if let Some(e) = unsupported() {
            return Err(e);
        }
        if have("wmctrl") {
            run(Command::new("wmctrl").args(["-i", "-c", &format!("0x{:x}", w.id)])).map(|_| ())
        } else {
            run(Command::new("xdotool").args(["windowclose", &w.id.to_string()])).map(|_| ())
        }
    }

    fn focus(&self, w: &WindowHandle) -> Result<(), MoverError> {
        if let Some(e) = unsupported() {
            return Err(e);
        }
        if have("wmctrl") {
            run(Command::new("wmctrl").args(["-i", "-a", &format!("0x{:x}", w.id)])).map(|_| ())
        } else {
            run(Command::new("xdotool").args(["windowactivate", &w.id.to_string()])).map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wmctrl_list() {
        let out = "0x01 0 host Other\n0x04200003  0 host tod design abc123\n";
        let w = parse_wmctrl_list(out, "tod design").unwrap();
        assert_eq!(w.id, 0x04200003);
        assert_eq!(w.title, "tod design abc123");
    }
}
