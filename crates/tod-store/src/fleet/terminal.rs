//! OS terminal launcher for agent shell sessions.

use crate::fleet::reconnect_identity::{self, ReconnectIdentity};
use crate::fleet::{FleetMutation, FleetStore, resolve_agent_workspace};
use crate::paths::TodPaths;
use crate::settings::{TerminalSettings, TodSettings};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Spawn an interactive terminal whose working directory is `cwd`.
///
/// Returns the child process handle when the platform provides a trackable PID
/// (PowerShell/bash direct spawn). Some frontends (e.g. Windows Terminal) detach
/// immediately and may not yield a long-lived child PID.
pub fn launch_terminal(cwd: &Path, settings: &TerminalSettings) -> Result<std::process::Child> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if !cwd.is_dir() {
        bail!("workspace directory does not exist: {}", cwd.display());
    }

    let program = settings
        .program
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::trim);

    if let Some(program) = program {
        return spawn_custom(program, &cwd);
    }

    #[cfg(windows)]
    {
        return spawn_windows_default(&cwd);
    }
    #[cfg(target_os = "macos")]
    {
        return spawn_macos_default(&cwd);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return spawn_linux_default(&cwd);
    }
}

fn spawn_custom(program: &str, cwd: &Path) -> Result<std::process::Child> {
    #[cfg(windows)]
    {
        if is_git_bash(program) {
            return Command::new(program)
                .arg(format!("--cd={}", cwd.display()))
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .with_context(|| format!("spawn Git Bash `{program}` in {}", cwd.display()));
        }
        if is_windows_terminal(program) {
            return Command::new(program)
                .arg("-w")
                .arg("0")
                .arg("new-tab")
                .arg("-d")
                .arg(cwd)
                .spawn()
                .with_context(|| {
                    format!("spawn Windows Terminal `{program}` in {}", cwd.display())
                });
        }
    }

    Command::new(program)
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawn terminal `{program}` in {}", cwd.display()))
}

#[cfg(windows)]
fn is_git_bash(program: &str) -> bool {
    program.eq_ignore_ascii_case("git-bash.exe")
        || Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("git-bash.exe"))
}

#[cfg(windows)]
fn is_windows_terminal(program: &str) -> bool {
    program.eq_ignore_ascii_case("wt.exe")
        || Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("wt.exe"))
}

#[cfg(windows)]
fn spawn_windows_default(cwd: &Path) -> Result<std::process::Child> {
    if command_available("wt.exe") {
        return Command::new("wt.exe")
            .arg("-w")
            .arg("0")
            .arg("new-tab")
            .arg("-d")
            .arg(cwd)
            .spawn()
            .context("spawn Windows Terminal (wt.exe)");
    }
    Command::new("powershell.exe")
        .arg("-NoExit")
        .arg("-NoLogo")
        .current_dir(cwd)
        .spawn()
        .context("spawn PowerShell")
}

#[cfg(target_os = "macos")]
fn spawn_macos_default(cwd: &Path) -> Result<std::process::Child> {
    let cwd_str = cwd.to_string_lossy();
    if command_available("iTerm.app") || std::path::Path::new("/Applications/iTerm.app").exists() {
        let script = format!(
            "tell application \"iTerm\"\n\
             activate\n\
             create window with default profile\n\
             tell current session of current window\n\
               write text \"cd {}\"\n\
             end tell\n\
             end tell",
            escape_applescript(&cwd_str)
        );
        return Command::new("osascript")
            .args(["-e", &script])
            .spawn()
            .context("spawn iTerm via osascript");
    }
    let script = format!(
        "tell application \"Terminal\"\n\
         activate\n\
         do script \"cd {}\"\n\
         end tell",
        escape_applescript(&cwd_str)
    );
    Command::new("osascript")
        .args(["-e", &script])
        .spawn()
        .context("spawn Terminal.app via osascript")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_linux_default(cwd: &Path) -> Result<std::process::Child> {
    for candidate in [
        "x-terminal-emulator",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "alacritty",
        "kitty",
    ] {
        if command_available(candidate) {
            return Command::new(candidate)
                .current_dir(cwd)
                .spawn()
                .with_context(|| format!("spawn {candidate}"));
        }
    }
    bail!("no terminal emulator found on PATH; set terminal.program in tod.yml")
}

fn command_available(name: &str) -> bool {
    #[cfg(windows)]
    {
        Command::new("where")
            .arg(name)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name} >/dev/null 2>&1"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn default_terminal_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Auto: Windows Terminal (wt.exe), else PowerShell"
    }
    #[cfg(target_os = "macos")]
    {
        "Auto: iTerm if installed, else Terminal.app"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "Auto: x-terminal-emulator, gnome-terminal, …"
    }
}

fn fresh_terminal_settings(paths: &TodPaths, fallback: &TerminalSettings) -> TerminalSettings {
    TodSettings::load(paths)
        .map(|settings| settings.terminal)
        .unwrap_or_else(|_| fallback.clone())
}

/// Open a shell session: resolve workspace, spawn terminal, persist session row.
pub fn open_shell_for_agent_config(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    config_id: &str,
    node_id: &str,
) -> Result<(String, PathBuf)> {
    let agent = fleet
        .get_agent(config_id)?
        .with_context(|| format!("agent config {config_id} not found"))?;
    let cwd = resolve_agent_workspace(fleet, paths, settings, &agent, node_id)?;
    let terminal = fresh_terminal_settings(paths, &settings.terminal);
    let child = launch_terminal(&cwd, &terminal)?;
    let reconnect = reconnect_identity::record(child.id());
    let shell_id = uuid::Uuid::new_v4().to_string();
    fleet.enqueue(FleetMutation::CreateShellSession {
        id: shell_id.clone(),
        agent_id: config_id.to_string(),
        reconnect,
    })?;
    fleet.writer().flush().context("persist shell session")?;
    Ok((shell_id, cwd))
}

/// Re-open a terminal in the agent workspace (focus existing shell from UI).
pub fn focus_shell_session(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    config_id: &str,
    node_id: &str,
    shell: &crate::fleet::ShellSession,
) -> Result<PathBuf> {
    if let Some(id) = shell.reconnect {
        if reconnect_identity::verify(id.pid, id.birth_token) {
            return Ok(resolve_agent_workspace(
                fleet,
                paths,
                settings,
                &fleet
                    .get_agent(config_id)?
                    .context("agent config not found")?,
                node_id,
            )?);
        }
    }
    let (_, cwd) = open_shell_for_agent_config(fleet, paths, settings, config_id, node_id)?;
    Ok(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_applescript_quotes() {
        assert_eq!(escape_applescript("C:\\dev\"test"), "C:\\dev\\\"test");
    }

    #[cfg(windows)]
    #[test]
    fn detects_git_bash_and_windows_terminal_by_name_or_path() {
        assert!(is_git_bash("git-bash.exe"));
        assert!(is_git_bash(r"C:\app\dev\Git\git-bash.exe"));
        assert!(!is_git_bash("powershell.exe"));
        assert!(is_windows_terminal("wt.exe"));
        assert!(is_windows_terminal(
            r"C:\Users\me\AppData\Local\Microsoft\WindowsApps\wt.exe"
        ));
    }
}
