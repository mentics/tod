//! OS terminal launcher for agent shell sessions.

mod focus;
mod init;
pub(crate) mod path_util;
mod state;

use crate::agent_launch::AgentLaunchOptions;
use crate::fleet::reconnect_identity::{self};
use crate::fleet::repos::agent_run::{AgentRun, RUNTIME_STATUS_ACTIVE};
use crate::fleet::repos::shell::ShellSession;
use crate::fleet::{FleetMutation, FleetStore, resolve_launch_cwd};
use crate::paths::TodPaths;
use crate::settings::{TerminalSettings, TodSettings};
use anyhow::{Context, Result, bail};
use focus::focus_shell_terminal;
#[cfg(any(unix, test))]
use init::posix_launch_command;
#[cfg(windows)]
use init::windows_launch_args;
use init::{ShellInitAssets, ensure_shell_init_assets};
#[cfg(windows)]
use init::{msys_path, write_session_init_script};
use path_util::normalize_launch_path;
use state::wait_for_shell_state;
pub use state::{
    ShellState, read_shell_state, remove_shell_state, shells_dir, state_file_path,
    write_shell_state,
};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Spawn an interactive terminal whose working directory is `cwd`.
///
/// The shell sources a bootstrap script that records the live shell PID in
/// `state_dir/{shell_id}.json` for focus and liveness checks.
///
/// When `startup_command` is set, the shell runs that command after bootstrap
/// (without `exec`), so the session remains if the command exits.
pub fn launch_shell_terminal(
    cwd: &Path,
    settings: &TerminalSettings,
    shell_id: &str,
    assets: &ShellInitAssets,
    startup_command: Option<&str>,
) -> Result<()> {
    let cwd = normalize_launch_path(cwd);
    if !cwd.is_dir() {
        bail!("workspace directory does not exist: {}", cwd.display());
    }
    let state_dir = normalize_launch_path(&assets.state_dir);
    let launch_assets = ShellInitAssets {
        state_dir: state_dir.clone(),
        posix_init: state_dir.join("tod-shell-init.sh"),
        #[cfg(windows)]
        windows_init: state_dir.join("tod-shell-init.ps1"),
    };

    let program = settings
        .program
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::trim);

    if let Some(program) = program {
        return spawn_custom(program, &cwd, shell_id, &launch_assets, startup_command);
    }

    #[cfg(windows)]
    {
        return spawn_windows_default(&cwd, shell_id, &launch_assets, startup_command);
    }
    #[cfg(target_os = "macos")]
    {
        return spawn_macos_default(&cwd, shell_id, &launch_assets, startup_command);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return spawn_linux_default(&cwd, shell_id, &launch_assets, startup_command);
    }
}

fn spawn_custom(
    program: &str,
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    startup_command: Option<&str>,
) -> Result<()> {
    #[cfg(windows)]
    {
        if is_git_bash(program) {
            return spawn_git_bash(program, &cwd, shell_id, assets, "git_bash", startup_command);
        }
        if is_windows_terminal(program) {
            return spawn_windows_terminal(
                program,
                cwd,
                shell_id,
                assets,
                "windows_terminal",
                startup_command,
            );
        }
        if is_powershell(program) {
            return spawn_powershell(
                program,
                cwd,
                shell_id,
                assets,
                "powershell",
                startup_command,
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Terminal.app and iTerm take no command-line program to run, so
        // settings like `open -a Terminal` or `iTerm` are driven through
        // AppleScript, the same as the default.
        if let Some(backend) = macos_terminal_backend(program) {
            return spawn_macos_terminal(cwd, shell_id, assets, backend, startup_command);
        }
    }

    #[cfg(unix)]
    {
        let cmd = posix_launch_command(
            &assets.posix_init,
            shell_id,
            &assets.state_dir,
            cwd,
            "posix",
            startup_command,
        );
        // The setting may carry its own arguments (`kitty --single-instance`).
        let mut words = program.split_whitespace();
        let exe = words.next().unwrap_or(program);
        return Command::new(exe)
            .current_dir(cwd)
            .args(words)
            .args(["-e", "bash", "-lc", &cmd])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .with_context(|| format!("spawn terminal `{program}` in {}", cwd.display()));
    }
    #[cfg(not(unix))]
    {
        let _ = startup_command;
        bail!(
            "custom terminal program `{program}` is not supported on this platform; \
             use wt.exe, powershell.exe, or git-bash.exe"
        );
    }
}

#[cfg(windows)]
fn find_mintty_pid_for_shell(shell_id: &str) -> Option<u32> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let needle = format!("{shell_id}-init.sh").replace('\'', "''");
    let script = format!(
        "$needle = '{needle}'; \
         Get-CimInstance Win32_Process -Filter \"Name='mintty.exe'\" | \
         Where-Object {{ $_.CommandLine -like \"*$needle*\" }} | \
         Select-Object -First 1 -ExpandProperty ProcessId"
    );
    let output = Command::new("powershell.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-Command", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    text.parse().ok()
}

#[cfg(windows)]
fn wait_for_mintty_session(shell_id: &str) -> Result<u32> {
    for _ in 0..50 {
        if let Some(pid) = find_mintty_pid_for_shell(shell_id) {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if reconnect_identity::pid_exists(pid) {
                return Ok(pid);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("timed out waiting for mintty process for session init")
}

#[cfg(windows)]
fn spawn_git_bash(
    program: &str,
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    backend: &str,
    startup_command: Option<&str>,
) -> Result<()> {
    let session_init = write_session_init_script(
        &assets.posix_init,
        &assets.state_dir,
        shell_id,
        cwd,
        backend,
        startup_command,
    )?;
    let session_init = msys_path(&session_init);
    let git_root = Path::new(program)
        .parent()
        .with_context(|| format!("resolve Git for Windows root from `{program}`"))?;
    let mintty = git_root.join("usr").join("bin").join("mintty.exe");
    if mintty.is_file() {
        // The terminal opens its own window. Inheriting our stdio would let the
        // shell hold a caller's pipe open long after this process exits.
        Command::new(mintty)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .args([
                "-h",
                "always",
                "-i",
                "/mingw64/share/git/git-for-windows.ico",
                "/bin/bash",
                "--init-file",
                &session_init,
                "-i",
            ])
            .current_dir(cwd)
            .spawn()
            .with_context(|| format!("spawn mintty for Git Bash in {}", cwd.display()))?;
        let pid = wait_for_mintty_session(shell_id)?;
        write_shell_state(&assets.state_dir, shell_id, pid, backend)?;
        return Ok(());
    }
    Command::new(program)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .arg(format!("--cd={}", cwd.display()))
        .arg("/bin/bash")
        .arg("--init-file")
        .arg(&session_init)
        .arg("-i")
        .spawn()
        .with_context(|| format!("spawn Git Bash `{program}` in {}", cwd.display()))?;
    let pid = wait_for_mintty_session(shell_id)?;
    write_shell_state(&assets.state_dir, shell_id, pid, backend)?;
    Ok(())
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
fn is_powershell(program: &str) -> bool {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    name.eq_ignore_ascii_case("powershell.exe") || name.eq_ignore_ascii_case("pwsh.exe")
}

#[cfg(windows)]
fn spawn_windows_terminal(
    program: &str,
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    backend: &str,
    startup_command: Option<&str>,
) -> Result<()> {
    let mut args = vec![
        "-w".into(),
        "-1".into(),
        "new-tab".into(),
        "-d".into(),
        cwd.display().to_string(),
        "powershell.exe".into(),
    ];
    args.extend(windows_launch_args(
        &assets.windows_init,
        shell_id,
        &assets.state_dir,
        cwd,
        backend,
        startup_command,
    ));
    Command::new(program)
        .env("TOD_TERMINAL_BACKEND", backend)
        .args(args)
        .spawn()
        .map(|_| ())
        .with_context(|| format!("spawn Windows Terminal `{program}` in {}", cwd.display()))
}

#[cfg(windows)]
fn spawn_powershell(
    program: &str,
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    backend: &str,
    startup_command: Option<&str>,
) -> Result<()> {
    // GUI hosts (tod.exe) are not console processes, and `CreateProcess` +
    // `CREATE_NEW_CONSOLE` ties the child's std handles to whatever the
    // caller's own (possibly redirected/piped, non-console) handles are —
    // which can make an interactive `-NoExit` PowerShell see EOF on stdin
    // and exit almost immediately. `ShellExecuteExW` launches the process
    // the way Explorer/"Run" does: fully detached from our own std handles,
    // always with a real console for the new process.
    let args = windows_launch_args(
        &assets.windows_init,
        shell_id,
        &assets.state_dir,
        cwd,
        backend,
        startup_command,
    );
    let params = join_windows_args(&args);
    shell_execute_new_console(program, cwd, &params)
        .with_context(|| format!("spawn PowerShell `{program}` in {}", cwd.display()))
}

#[cfg(windows)]
fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return arg.to_string();
    }
    // https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw
    // parameters are parsed with the same argv quoting rules as CommandLineToArgvW.
    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(ch);
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(windows)]
fn join_windows_args(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_windows_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
fn shell_execute_new_console(program: &str, cwd: &Path, params: &str) -> Result<()> {
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
    use windows::core::PCWSTR;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    // Tests launch real consoles and must not take focus from whoever is
    // using the machine. Only a hidden window reliably avoids it: a
    // default-terminal handoff to Windows Terminal ignores minimized/
    // no-activate requests (hidden skips the handoff), and so does
    // `conhost.exe` launched directly. The hidden console still exists, so
    // callers can still check that a console window was created.
    let show = if cfg!(test) { SW_HIDE } else { SW_SHOWNORMAL };

    let file = wide(program);
    let params_w = wide(params);
    let dir = wide(&cwd.display().to_string());

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params_w.as_ptr()),
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: show.0,
        ..Default::default()
    };

    if unsafe { ShellExecuteExW(&mut info) }.is_err() || info.hProcess.is_invalid() {
        bail!("ShellExecuteExW failed to launch `{program}`");
    }
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
    }
    Ok(())
}

#[cfg(windows)]
fn spawn_windows_default(
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    startup_command: Option<&str>,
) -> Result<()> {
    // Go straight to powershell.exe rather than wt.exe: `wt` re-tokenizes and
    // re-quotes the trailing command line itself (it's not a plain CreateProcess
    // argv pass-through), which corrupts a startup command containing quotes/
    // spaces (e.g. a `--name "..."` with an embedded space); it also opens as a
    // new tab in the last-used window (`-w 0`) rather than a new window.
    // `spawn_powershell` passes args straight through via `Command::args` (no
    // re-tokenizing) and always opens a new console window.
    spawn_powershell(
        "powershell.exe",
        cwd,
        shell_id,
        assets,
        "powershell",
        startup_command,
    )
}

#[cfg(target_os = "macos")]
fn spawn_macos_default(
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    startup_command: Option<&str>,
) -> Result<()> {
    spawn_macos_terminal(cwd, shell_id, assets, "macos_terminal", startup_command)
}

/// The AppleScript backend for a terminal setting naming Terminal.app or
/// iTerm, in any of the forms a user types: `Terminal`, `Terminal.app`,
/// `open -a Terminal`, `/Applications/iTerm.app`, `open -a iTerm2`.
#[cfg(any(target_os = "macos", test))]
fn macos_terminal_backend(program: &str) -> Option<&'static str> {
    let mut words: Vec<&str> = program.split_whitespace().collect();
    if words.first() == Some(&"open") {
        words.retain(|w| !w.starts_with('-') && *w != "open");
    }
    let [app] = words.as_slice() else {
        return None;
    };
    let name = app.trim_end_matches('/').rsplit('/').next().unwrap_or(app);
    let name = name.trim_end_matches(".app").to_ascii_lowercase();
    match name.as_str() {
        "terminal" => Some("macos_terminal"),
        "iterm" | "iterm2" => Some("iterm"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn spawn_macos_terminal(
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    backend: &str,
    startup_command: Option<&str>,
) -> Result<()> {
    let launch_cmd = posix_launch_command(
        &assets.posix_init,
        shell_id,
        &assets.state_dir,
        cwd,
        backend,
        startup_command,
    );
    let escaped_cmd = escape_applescript(&launch_cmd);
    let script = if backend == "iterm" {
        format!(
            "tell application \"iTerm\"\n\
             activate\n\
             create window with default profile\n\
             tell current session of current window\n\
               write text \"{escaped_cmd}\"\n\
             end tell\n\
             end tell"
        )
    } else {
        format!(
            "tell application \"Terminal\"\n\
             activate\n\
             do script \"{escaped_cmd}\"\n\
             end tell"
        )
    };
    run_osascript(&script)
}

#[cfg(target_os = "macos")]
fn run_osascript(script: &str) -> Result<()> {
    let status = Command::new("osascript")
        .args(["-e", script])
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

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_linux_default(
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    startup_command: Option<&str>,
) -> Result<()> {
    let cmd = posix_launch_command(
        &assets.posix_init,
        shell_id,
        &assets.state_dir,
        cwd,
        "posix",
        startup_command,
    );
    for candidate in [
        "x-terminal-emulator",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "alacritty",
        "kitty",
    ] {
        if command_available(candidate) {
            return match candidate {
                "gnome-terminal" | "xfce4-terminal" => Command::new(candidate)
                    .args(["--", "bash", "-lc", &cmd])
                    .current_dir(cwd)
                    .spawn()
                    .map(|_| ()),
                "konsole" => Command::new(candidate)
                    .args(["-e", "bash", "-lc", &cmd])
                    .current_dir(cwd)
                    .spawn()
                    .map(|_| ()),
                _ => Command::new(candidate)
                    .arg("-e")
                    .arg("bash")
                    .arg("-lc")
                    .arg(&cmd)
                    .current_dir(cwd)
                    .spawn()
                    .map(|_| ()),
            }
            .with_context(|| format!("spawn {candidate}"));
        }
    }
    bail!("no terminal emulator found on PATH; set terminal.program in tod.yml")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn command_available(name: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn default_terminal_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Auto: PowerShell (new window). Set to wt.exe for Windows Terminal instead."
    }
    #[cfg(target_os = "macos")]
    {
        "Auto: Terminal.app. Set to iTerm for iTerm instead."
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

fn shell_state_for_session(paths: &TodPaths, shell_id: &str) -> Option<ShellState> {
    let path = state_file_path(&shells_dir(paths), shell_id);
    read_shell_state(&path).ok()
}

fn reconnect_for_shell(
    paths: &TodPaths,
    shell: &ShellSession,
) -> Option<reconnect_identity::ReconnectIdentity> {
    if let Some(state) = shell_state_for_session(paths, &shell.id) {
        if let Some(id) = reconnect_identity::record(state.pid) {
            return Some(id);
        }
    }
    shell.reconnect
}

fn shell_is_alive(paths: &TodPaths, shell: &ShellSession) -> bool {
    if let Some(id) = reconnect_for_shell(paths, shell) {
        if reconnect_identity::verify(id.pid, id.birth_token) {
            return true;
        }
    }
    shell_state_for_session(paths, &shell.id)
        .is_some_and(|state| reconnect_identity::pid_exists(state.pid))
}

/// Check whether a shell session's tracked process is still alive.
pub fn verify_shell_session(paths: &TodPaths, shell_id: &str) -> Result<(bool, Option<u32>)> {
    let path = state_file_path(&shells_dir(paths), shell_id);
    let state = read_shell_state(&path).ok();
    let pid = state.as_ref().map(|s| s.pid);
    for attempt in 0..10 {
        if let Some(state) = state.as_ref() {
            if let Some(id) = reconnect_identity::record(state.pid) {
                if reconnect_identity::verify(id.pid, id.birth_token) {
                    return Ok((true, pid));
                }
            }
            if reconnect_identity::pid_exists(state.pid) {
                return Ok((true, pid));
            }
        }
        if attempt + 1 < 10 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    Ok((false, pid))
}

/// Remove shell sessions whose tracked process is no longer alive.
pub fn prune_stale_shell_sessions(
    fleet: &FleetStore,
    paths: &TodPaths,
    node_id: &str,
) -> Result<usize> {
    let shells = fleet.list_shells_for_node(node_id)?;
    let mut removed = 0usize;
    for shell in shells {
        if shell_is_alive(paths, &shell) {
            continue;
        }
        fleet.enqueue(FleetMutation::DismissShellSession {
            id: shell.id.clone(),
        })?;
        remove_shell_state(paths, &shell.id);
        removed += 1;
    }
    if removed > 0 {
        fleet.writer().flush().context("persist shell pruning")?;
    }
    Ok(removed)
}

fn tracked_session_alive(
    paths: &TodPaths,
    session_id: &str,
    reconnect: Option<&reconnect_identity::ReconnectIdentity>,
) -> bool {
    if let Some(state) = shell_state_for_session(paths, session_id) {
        if let Some(id) = reconnect_identity::record(state.pid) {
            if reconnect_identity::verify(id.pid, id.birth_token) {
                return true;
            }
        }
        if reconnect_identity::pid_exists(state.pid) {
            return true;
        }
    }
    if let Some(id) = reconnect {
        if reconnect_identity::verify(id.pid, id.birth_token) {
            return true;
        }
    }
    false
}

fn terminal_agent_is_alive(paths: &TodPaths, run: &AgentRun) -> bool {
    tracked_session_alive(paths, &run.id, run.reconnect.as_ref())
}

/// Delete terminal agent runs whose OS process is gone; clear state files.
pub fn prune_stale_terminal_agent_runs(
    fleet: &FleetStore,
    paths: &TodPaths,
    node_id: &str,
) -> Result<usize> {
    let runs = fleet.list_terminal_agent_runs_for_node(node_id)?;
    let mut removed = 0usize;
    for run in runs {
        if terminal_agent_is_alive(paths, &run) {
            continue;
        }
        fleet.enqueue(FleetMutation::DeleteAgentRun {
            run_id: run.id.clone(),
        })?;
        remove_shell_state(paths, &run.id);
        removed += 1;
    }
    if removed > 0 {
        fleet
            .writer()
            .flush()
            .context("persist terminal agent pruning")?;
    }
    Ok(removed)
}

/// Open a shell in the node's resolved Files directory and persist the session
/// row; optionally run `startup_command` after bootstrap.
pub fn open_shell_for_node(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
    startup_command: Option<&str>,
) -> Result<(String, PathBuf)> {
    let cwd = resolve_launch_cwd(fleet, node_id)?;
    let terminal = fresh_terminal_settings(paths, &settings.terminal);
    let assets = ensure_shell_init_assets(paths)?;
    let shell_id = uuid::Uuid::new_v4().to_string();
    launch_shell_terminal(&cwd, &terminal, &shell_id, &assets, startup_command)?;
    let state_dir = normalize_launch_path(&assets.state_dir);
    let state = wait_for_shell_state(&state_dir, &shell_id)?;
    let reconnect = reconnect_identity::record(state.pid);
    fleet.enqueue(FleetMutation::CreateShellSession {
        id: shell_id.clone(),
        node_id: node_id.to_string(),
        reconnect,
    })?;
    fleet.writer().flush().context("persist shell session")?;
    Ok((shell_id, cwd))
}

/// Launch the agent CLI in an OS terminal, tracked as a `terminal` agent run (not a shell).
///
/// Uses the agent run id as the terminal state-file key for PID focus/liveness.
pub fn open_terminal_agent_for_node(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
    startup_command: &str,
    launch: Option<AgentLaunchOptions>,
) -> Result<(String, PathBuf)> {
    let cwd = resolve_launch_cwd(fleet, node_id)?;

    fleet.enqueue(FleetMutation::CreateAgentRun {
        node_id: node_id.to_string(),
        run_kind: Some("terminal".into()),
        session_name: None,
        launch,
    })?;
    fleet
        .writer()
        .flush()
        .context("persist terminal agent run")?;
    let _ = fleet.reload_if_stale();

    let run_id = fleet
        .list_terminal_agent_runs_for_node(node_id)?
        .into_iter()
        .next()
        .map(|run| run.id)
        .with_context(|| format!("terminal agent run not created for {node_id}"))?;

    let terminal = fresh_terminal_settings(paths, &settings.terminal);
    let assets = ensure_shell_init_assets(paths)?;
    if let Err(err) =
        launch_shell_terminal(&cwd, &terminal, &run_id, &assets, Some(startup_command))
    {
        let _ = fleet.enqueue(FleetMutation::DeleteAgentRun {
            run_id: run_id.clone(),
        });
        let _ = fleet.writer().flush();
        return Err(err);
    }

    let state_dir = normalize_launch_path(&assets.state_dir);
    let state = match wait_for_shell_state(&state_dir, &run_id) {
        Ok(state) => state,
        Err(err) => {
            let _ = fleet.enqueue(FleetMutation::DeleteAgentRun {
                run_id: run_id.clone(),
            });
            let _ = fleet.writer().flush();
            return Err(err);
        }
    };
    let reconnect = reconnect_identity::record(state.pid)
        .with_context(|| format!("record reconnect identity for terminal agent {run_id}"))?;

    fleet.enqueue(FleetMutation::UpdateAgentRunReconnect {
        run_id: run_id.clone(),
        identity: reconnect,
    })?;
    fleet.enqueue(FleetMutation::UpdateAgentRunRuntimeStatus {
        run_id: run_id.clone(),
        runtime_status: RUNTIME_STATUS_ACTIVE.into(),
    })?;
    fleet
        .writer()
        .flush()
        .context("persist terminal agent reconnect")?;
    Ok((run_id, cwd))
}

/// Focus a terminal agent window, or relaunch the CLI (with the run's recorded
/// launch options) when the process is gone.
pub fn focus_terminal_agent_run(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    run: &AgentRun,
    startup_command: &str,
) -> Result<PathBuf> {
    let cwd = resolve_launch_cwd(fleet, &run.node_id)?;

    if terminal_agent_is_alive(paths, run) {
        if let Some(state) = shell_state_for_session(paths, &run.id) {
            focus_shell_terminal(&state).context("focus existing terminal agent")?;
        }
        return Ok(cwd);
    }

    remove_shell_state(paths, &run.id);
    fleet.enqueue(FleetMutation::DeleteAgentRun {
        run_id: run.id.clone(),
    })?;
    fleet
        .writer()
        .flush()
        .context("delete stale terminal agent run")?;

    let (_, new_cwd) = open_terminal_agent_for_node(
        fleet,
        paths,
        settings,
        &run.node_id,
        startup_command,
        run.launch_options(),
    )?;
    Ok(new_cwd)
}

/// Focus an existing shell terminal, or open a new one when the process is gone.
pub fn focus_shell_session(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    shell: &ShellSession,
) -> Result<PathBuf> {
    let cwd = resolve_launch_cwd(fleet, &shell.node_id)?;

    if shell_is_alive(paths, shell) {
        if let Some(state) = shell_state_for_session(paths, &shell.id) {
            focus_shell_terminal(&state).context("focus existing shell terminal")?;
        }
        return Ok(cwd);
    }

    remove_shell_state(paths, &shell.id);
    fleet.enqueue(FleetMutation::DismissShellSession {
        id: shell.id.clone(),
    })?;
    fleet
        .writer()
        .flush()
        .context("dismiss stale shell session")?;

    let (_, new_cwd) = open_shell_for_node(fleet, paths, settings, &shell.node_id, None)?;
    Ok(new_cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn macos_terminal_backend_recognizes_app_forms() {
        for s in [
            "open -a Terminal",
            "Terminal",
            "Terminal.app",
            "/System/Applications/Utilities/Terminal.app",
        ] {
            assert_eq!(macos_terminal_backend(s), Some("macos_terminal"), "{s}");
        }
        for s in ["open -a iTerm", "iTerm2", "/Applications/iTerm.app/"] {
            assert_eq!(macos_terminal_backend(s), Some("iterm"), "{s}");
        }
        for s in ["kitty", "alacritty --option x", "open -a Kitty"] {
            assert_eq!(macos_terminal_backend(s), None, "{s}");
        }
    }

    #[test]
    fn posix_launch_command_quotes_paths() {
        let cmd = posix_launch_command(
            Path::new("/tmp/init.sh"),
            "shell-id",
            Path::new("/tmp/state"),
            Path::new("/tmp/work space"),
            "macos_terminal",
            None,
        );
        assert!(cmd.contains("'/tmp/work space'"));
        assert!(cmd.contains("TOD_TERMINAL_BACKEND=macos_terminal"));
    }

    #[test]
    fn posix_launch_command_appends_startup() {
        let cmd = posix_launch_command(
            Path::new("/tmp/init.sh"),
            "shell-id",
            Path::new("/tmp/state"),
            Path::new("/tmp/cwd"),
            "posix",
            Some("claude"),
        );
        assert!(cmd.ends_with("; claude") || cmd.contains("; claude"));
    }

    /// When dropped — including when an assertion fails — kills every process
    /// launched for a terminal session, matched by the shell id in its command
    /// line. `taskkill /T` from the recorded pid misses Git Bash's `bash.exe`,
    /// whose Windows parent has already exited; left running it holds the test
    /// runner's output pipe open and hangs any piped `cargo test`.
    #[cfg(windows)]
    struct KillShellSession(String);

    #[cfg(windows)]
    impl Drop for KillShellSession {
        fn drop(&mut self) {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let needle = self.0.replace('\'', "''");
            let script = format!(
                "$needle = '{needle}'; \
                 Get-CimInstance Win32_Process | \
                 Where-Object {{ $_.ProcessId -ne $PID -and $_.CommandLine -like \"*$needle*\" }} | \
                 ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}"
            );
            let _ = std::process::Command::new("powershell.exe")
                .creation_flags(CREATE_NO_WINDOW)
                .args(["-NoProfile", "-Command", &script])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore]
    fn git_bash_launch_stays_alive() {
        use crate::paths::{clear_data_root_override, set_data_root};
        use crate::settings::TerminalSettings;
        use reconnect_identity;

        let root = std::env::temp_dir().join(format!("tod-shell-gb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        set_data_root(root.clone());
        let paths = crate::paths::TodPaths::discover().unwrap();
        let assets = ensure_shell_init_assets(&paths).unwrap();
        let cwd = normalize_launch_path(std::env::current_dir().unwrap().as_path());
        let shell_id = uuid::Uuid::new_v4().to_string();
        let _kill_session = KillShellSession(shell_id.clone());
        let settings = TerminalSettings {
            program: Some(r"C:\app\dev\Git\git-bash.exe".into()),
            ..TerminalSettings::default()
        };

        launch_shell_terminal(&cwd, &settings, &shell_id, &assets, None).unwrap();
        let state_dir = normalize_launch_path(&assets.state_dir);
        let state = wait_for_shell_state(&state_dir, &shell_id).unwrap();
        assert!(state.pid > 0, "expected mintty pid in shell state");
        assert_eq!(state.backend, "git_bash");
        assert!(
            reconnect_identity::pid_exists(state.pid),
            "git bash mintty pid {} should stay alive",
            state.pid
        );

        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &state.pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        clear_data_root_override();
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn powershell_launch_gets_visible_console() {
        use crate::paths::{clear_data_root_override, set_data_root};
        use crate::settings::TerminalSettings;
        use reconnect_identity;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn FreeConsole() -> i32;
            fn GetConsoleWindow() -> *mut core::ffi::c_void;
        }

        let root = std::env::temp_dir().join(format!("tod-shell-ps-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        set_data_root(root.clone());
        let paths = crate::paths::TodPaths::discover().unwrap();
        let assets = ensure_shell_init_assets(&paths).unwrap();
        let cwd = normalize_launch_path(std::env::current_dir().unwrap().as_path());
        let shell_id = uuid::Uuid::new_v4().to_string();
        let _kill_session = KillShellSession(shell_id.clone());
        let settings = TerminalSettings {
            program: Some("powershell.exe".into()),
            ..TerminalSettings::default()
        };

        // Mimic tod.exe (Windows GUI subsystem): no attached console when spawning.
        // Without CREATE_NEW_CONSOLE, powershell starts headless (hwnd=0).
        if unsafe { !GetConsoleWindow().is_null() } {
            assert_ne!(unsafe { FreeConsole() }, 0, "FreeConsole failed");
        }

        let launch_result = launch_shell_terminal(&cwd, &settings, &shell_id, &assets, None);
        let state_dir = normalize_launch_path(&assets.state_dir);
        let state_result = launch_result.and_then(|_| wait_for_shell_state(&state_dir, &shell_id));

        let cleanup = |pid: Option<u32>| {
            if let Some(pid) = pid {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            clear_data_root_override();
            let _ = std::fs::remove_dir_all(&root);
        };

        let state = match state_result {
            Ok(state) => state,
            Err(err) => {
                cleanup(None);
                panic!("powershell launch/register failed: {err:#}");
            }
        };
        assert!(state.pid > 0, "expected powershell pid in shell state");
        assert_eq!(state.backend, "powershell");
        assert!(
            reconnect_identity::pid_exists(state.pid),
            "powershell pid {} should stay alive",
            state.pid
        );
        let hwnd = state.hwnd.unwrap_or(0);
        if hwnd == 0 {
            cleanup(Some(state.pid));
            panic!(
                "powershell should own a console window (hwnd), got 0; \
                 GUI hosts must spawn with CREATE_NEW_CONSOLE"
            );
        }

        cleanup(Some(state.pid));
    }

    /// A task with Files enabled and its workspace directory set to `cwd`.
    fn insert_files_task(
        store: &crate::fleet::store::FleetStore,
        title: &str,
        slug: &str,
        cwd: &Path,
    ) -> String {
        use crate::fleet::repos::task::FleetTask;
        use crate::fleet::writer::FleetMutation;
        use crate::outline::OutlineMutation;
        use crate::outline::types::Capability;

        let task_id = uuid::Uuid::new_v4().to_string();
        store
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(&task_id, title, slug),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: uuid::Uuid::parse_str(&task_id).unwrap(),
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        store
            .enqueue(FleetMutation::UpdateTaskRepo {
                id: task_id.clone(),
                repo: Some(cwd.display().to_string()),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let _ = store.reload_if_stale();
        task_id
    }

    #[test]
    fn prune_stale_terminal_agent_runs_deletes_dead_runs() {
        use crate::fleet::reconnect_identity::ReconnectIdentity;
        use crate::fleet::store::FleetStore;
        use crate::fleet::test_util::{cleanup_fleet_root, temp_fleet_root};
        use crate::fleet::writer::FleetMutation;
        use crate::paths::{clear_data_root_override, set_data_root};

        let fleet_root = temp_fleet_root();
        let store = FleetStore::open(&fleet_root).unwrap();
        set_data_root(fleet_root.clone());
        let paths = crate::paths::TodPaths::discover().unwrap();

        let node_id = insert_files_task(
            &store,
            "Terminal prune",
            "terminal-prune",
            &std::env::current_dir().unwrap(),
        );

        store
            .enqueue(FleetMutation::CreateAgentRun {
                node_id: node_id.clone(),
                run_kind: Some("terminal".into()),
                session_name: None,
                launch: None,
            })
            .unwrap();
        store.writer().flush().unwrap();
        let _ = store.reload_if_stale();
        let run = store
            .list_terminal_agent_runs_for_node(&node_id)
            .unwrap()
            .into_iter()
            .next()
            .expect("terminal run");
        store
            .enqueue(FleetMutation::UpdateAgentRunReconnect {
                run_id: run.id.clone(),
                identity: ReconnectIdentity {
                    pid: 4_294_967_294,
                    birth_token: 1,
                },
            })
            .unwrap();
        store
            .enqueue(FleetMutation::UpdateAgentRunRuntimeStatus {
                run_id: run.id.clone(),
                runtime_status: RUNTIME_STATUS_ACTIVE.into(),
            })
            .unwrap();
        store.writer().flush().unwrap();

        let removed = prune_stale_terminal_agent_runs(&store, &paths, &node_id).unwrap();
        assert_eq!(removed, 1);
        let _ = store.reload_if_stale();
        assert!(
            store
                .list_terminal_agent_runs_for_node(&node_id)
                .unwrap()
                .is_empty(),
            "dead processing run should be deleted, not kept as not_running"
        );

        store
            .enqueue(FleetMutation::CreateAgentRun {
                node_id: node_id.clone(),
                run_kind: Some("terminal".into()),
                session_name: None,
                launch: None,
            })
            .unwrap();
        store.writer().flush().unwrap();
        let _ = store.reload_if_stale();
        let ended = store
            .list_terminal_agent_runs_for_node(&node_id)
            .unwrap()
            .into_iter()
            .next()
            .expect("second terminal run");
        store
            .enqueue(FleetMutation::EndAgentRun {
                run_id: ended.id.clone(),
            })
            .unwrap();
        store.writer().flush().unwrap();

        let removed = prune_stale_terminal_agent_runs(&store, &paths, &node_id).unwrap();
        assert_eq!(removed, 1);
        let _ = store.reload_if_stale();
        assert!(
            store
                .list_terminal_agent_runs_for_node(&node_id)
                .unwrap()
                .is_empty(),
            "already-ended terminal runs should be deleted on prune"
        );

        clear_data_root_override();
        cleanup_fleet_root(&fleet_root);
    }

    #[cfg(windows)]
    #[test]
    fn open_shell_for_node_registers_live_process() {
        use crate::fleet::store::FleetStore;
        use crate::fleet::test_util::{cleanup_fleet_root, temp_fleet_root};
        use crate::paths::{clear_data_root_override, set_data_root};
        use crate::settings::TodSettings;
        use reconnect_identity;

        let fleet_root = temp_fleet_root();
        let store = FleetStore::open(&fleet_root).unwrap();
        set_data_root(fleet_root.clone());
        let paths = crate::paths::TodPaths::discover().unwrap();

        // Registered before the launch call (whose shell id isn't known until
        // it returns) by the fleet root's unique path instead, which appears
        // in the child's command line (`-TodStateDir <fleet_root>\shells`)
        // regardless of backend, so a panic mid-launch still gets cleaned up.
        let _kill_session = KillShellSession(
            fleet_root
                .file_name()
                .and_then(|name| name.to_str())
                .expect("fleet root has a name")
                .to_string(),
        );

        let cwd = std::env::current_dir().unwrap();
        let node_id = insert_files_task(&store, "Shell test", "shell-test", &cwd);

        let settings = TodSettings::default();
        let (shell_id, resolved) =
            open_shell_for_node(&store, &paths, &settings, &node_id, None).unwrap();
        assert_eq!(resolved, cwd);

        let (alive, pid) = verify_shell_session(&paths, &shell_id).unwrap();
        assert!(alive, "shell process should be alive after launch");
        let pid = pid.expect("pid in state file");
        let identity = reconnect_identity::record(pid).expect("pid visible to sysinfo");
        assert!(reconnect_identity::verify(
            identity.pid,
            identity.birth_token
        ));

        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        clear_data_root_override();
        cleanup_fleet_root(&fleet_root);
    }
}
