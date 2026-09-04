//! Bring an existing shell terminal window to the foreground.

use super::state::ShellState;
use anyhow::{Context, Result, bail};
use std::process::{Command, Stdio};

pub fn focus_shell_terminal(state: &ShellState) -> Result<()> {
    match state.backend.as_str() {
        "macos_terminal" => focus_macos_terminal(state),
        "iterm" => focus_iterm(state),
        "windows_terminal" | "powershell" | "windows" => focus_windows(state),
        "git_bash" => focus_git_bash(state),
        "posix" => focus_posix(state),
        other => {
            #[cfg(target_os = "macos")]
            {
                if focus_macos_terminal(state).is_ok() || focus_iterm(state).is_ok() {
                    return Ok(());
                }
            }
            if cfg!(unix) {
                focus_posix(state)
            } else if cfg!(windows) {
                focus_windows(state)
            } else {
                bail!("unsupported shell backend: {other}")
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn focus_macos_terminal(state: &ShellState) -> Result<()> {
    let Some(tty) = state.tty.as_deref().filter(|tty| !tty.is_empty()) else {
        return activate_app("Terminal");
    };
    let escaped_tty = escape_applescript(tty);
    let script = format!(
        "tell application \"Terminal\"\n\
         activate\n\
         repeat with w in windows\n\
           repeat with t in tabs of w\n\
             if tty of t is \"{escaped_tty}\" then\n\
               set selected of t to true\n\
               set index of w to 1\n\
               return\n\
             end if\n\
           end repeat\n\
         end repeat\n\
         end tell"
    );
    run_osascript(&script).context("focus Terminal.app tab")
}

#[cfg(not(target_os = "macos"))]
fn focus_macos_terminal(_state: &ShellState) -> Result<()> {
    bail!("Terminal.app focus is only supported on macOS")
}

#[cfg(target_os = "macos")]
fn focus_iterm(state: &ShellState) -> Result<()> {
    let Some(tty) = state.tty.as_deref().filter(|tty| !tty.is_empty()) else {
        return activate_app("iTerm");
    };
    let escaped_tty = escape_applescript(tty);
    let script = format!(
        "tell application \"iTerm\"\n\
         activate\n\
         repeat with w in windows\n\
           repeat with t in tabs of w\n\
             repeat with s in sessions of t\n\
               if tty of s is \"{escaped_tty}\" then\n\
                 select s\n\
                 set current window to w\n\
                 return\n\
               end if\n\
             end repeat\n\
           end repeat\n\
         end repeat\n\
         end tell"
    );
    run_osascript(&script).context("focus iTerm session")
}

#[cfg(not(target_os = "macos"))]
fn focus_iterm(_state: &ShellState) -> Result<()> {
    bail!("iTerm focus is only supported on macOS")
}

#[cfg(target_os = "macos")]
fn activate_app(name: &str) -> Result<()> {
    let script = format!("tell application \"{name}\" to activate");
    run_osascript(&script)
}

#[cfg(target_os = "macos")]
fn run_osascript(script: &str) -> Result<()> {
    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status()
        .context("run osascript")?;
    if status.success() {
        Ok(())
    } else {
        bail!("osascript exited with {status}");
    }
}

#[cfg(target_os = "macos")]
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(windows)]
fn focus_windows(state: &ShellState) -> Result<()> {
    let hwnd = state
        .hwnd
        .filter(|hwnd| *hwnd != 0)
        .ok_or_else(|| anyhow::anyhow!("shell state has no window handle"))?;
    focus_windows_hwnd(hwnd)
}

#[cfg(windows)]
fn focus_windows_by_pid(pid: u32) -> Result<()> {
    let script = format!(
        "$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue; if (-not $p) {{ exit 1 }}; \
         $h = $p.MainWindowHandle; if ($h -eq [IntPtr]::Zero) {{ exit 2 }}; \
         Add-Type @'\n\
using System;\n\
using System.Runtime.InteropServices;\n\
public class TodFocus {{\n\
  [DllImport(\"user32.dll\")] public static extern bool SetForegroundWindow(IntPtr hWnd);\n\
  [DllImport(\"user32.dll\")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);\n\
  [DllImport(\"user32.dll\")] public static extern bool BringWindowToTop(IntPtr hWnd);\n\
}}\n\
'@;\n\
[void][TodFocus]::ShowWindow($h, 9);\n\
[void][TodFocus]::BringWindowToTop($h);\n\
[void][TodFocus]::SetForegroundWindow($h);"
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("focus shell window by pid via PowerShell")?;
    if status.success() {
        Ok(())
    } else {
        bail!("PowerShell focus by pid exited with {status}");
    }
}

#[cfg(windows)]
fn focus_windows_hwnd(hwnd: u64) -> Result<()> {
    let script = format!(
        "Add-Type @'\n\
using System;\n\
using System.Runtime.InteropServices;\n\
public class TodFocus {{\n\
  [DllImport(\"user32.dll\")] public static extern bool SetForegroundWindow(IntPtr hWnd);\n\
  [DllImport(\"user32.dll\")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);\n\
  [DllImport(\"user32.dll\")] public static extern bool BringWindowToTop(IntPtr hWnd);\n\
}}\n\
'@;\n\
$h = [IntPtr]{hwnd};\n\
[void][TodFocus]::ShowWindow($h, 9);\n\
[void][TodFocus]::BringWindowToTop($h);\n\
[void][TodFocus]::SetForegroundWindow($h);"
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("focus shell window via PowerShell")?;
    if status.success() {
        Ok(())
    } else {
        bail!("PowerShell focus exited with {status}");
    }
}

#[cfg(not(windows))]
fn focus_windows(_state: &ShellState) -> Result<()> {
    bail!("Windows focus is only supported on Windows")
}

#[cfg(windows)]
fn focus_git_bash(state: &ShellState) -> Result<()> {
    if state.hwnd.filter(|hwnd| *hwnd != 0).is_some() {
        return focus_windows(state);
    }
    focus_windows_by_pid(state.pid)
}

#[cfg(not(windows))]
fn focus_git_bash(state: &ShellState) -> Result<()> {
    focus_posix(state)
}

#[cfg(unix)]
fn focus_posix(state: &ShellState) -> Result<()> {
    let output = Command::new("xdotool")
        .args(["search", "--pid", &state.pid.to_string()])
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            if let Some(window_id) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                let status = Command::new("xdotool")
                    .args(["windowactivate", "--sync", window_id])
                    .status()
                    .context("activate shell window with xdotool")?;
                if status.success() {
                    return Ok(());
                }
            }
        }
    }
    bail!("no supported focus tool found for pid {}", state.pid)
}

#[cfg(not(unix))]
fn focus_posix(_state: &ShellState) -> Result<()> {
    bail!("POSIX focus is only supported on Unix")
}

#[cfg(test)]
mod tests {
    use crate::fleet::terminal::init::sh_quote;

    #[cfg(target_os = "macos")]
    #[test]
    fn escape_applescript_quotes() {
        use super::escape_applescript;
        assert_eq!(escape_applescript("C:\\dev\"test"), "C:\\dev\\\"test");
    }

    #[test]
    fn sh_quote_handles_spaces_and_quotes() {
        assert_eq!(sh_quote("hello"), "'hello'");
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    }
}
