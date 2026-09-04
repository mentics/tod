//! OS terminal launcher for agent shell sessions.

mod focus;
mod init;
mod path_util;
mod state;

use crate::fleet::reconnect_identity::{self};
use crate::fleet::repos::shell::ShellSession;
use crate::fleet::{FleetMutation, FleetStore, resolve_agent_workspace};
use crate::paths::TodPaths;
use crate::settings::{TerminalSettings, TodSettings};
use anyhow::{Context, Result, bail};
use focus::focus_shell_terminal;
#[cfg(windows)]
use init::windows_launch_args;
use init::{
    ShellInitAssets, ensure_shell_init_assets, msys_path, posix_launch_command,
    write_session_init_script,
};
use path_util::normalize_launch_path;
use state::wait_for_shell_state;
pub use state::{
    ShellState, read_shell_state, remove_shell_state, shells_dir, state_file_path,
    write_shell_state,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Spawn an interactive terminal whose working directory is `cwd`.
///
/// The shell sources a bootstrap script that records the live shell PID in
/// `state_dir/{shell_id}.json` for focus and liveness checks.
pub fn launch_shell_terminal(
    cwd: &Path,
    settings: &TerminalSettings,
    shell_id: &str,
    assets: &ShellInitAssets,
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
        return spawn_custom(program, &cwd, shell_id, &launch_assets);
    }

    #[cfg(windows)]
    {
        return spawn_windows_default(&cwd, shell_id, &launch_assets);
    }
    #[cfg(target_os = "macos")]
    {
        return spawn_macos_default(&cwd, shell_id, &launch_assets);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return spawn_linux_default(&cwd, shell_id, &launch_assets);
    }
}

fn spawn_custom(program: &str, cwd: &Path, shell_id: &str, assets: &ShellInitAssets) -> Result<()> {
    #[cfg(windows)]
    {
        if is_git_bash(program) {
            return spawn_git_bash(program, &cwd, shell_id, assets, "git_bash");
        }
        if is_windows_terminal(program) {
            return spawn_windows_terminal(program, cwd, shell_id, assets, "windows_terminal");
        }
        if is_powershell(program) {
            return spawn_powershell(program, cwd, shell_id, assets, "powershell");
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
        );
        return Command::new(program)
            .current_dir(cwd)
            .args(["-e", "bash", "-lc", &cmd])
            .spawn()
            .map(|_| ())
            .with_context(|| format!("spawn terminal `{program}` in {}", cwd.display()));
    }
    #[cfg(not(unix))]
    {
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
) -> Result<()> {
    let session_init = write_session_init_script(
        &assets.posix_init,
        &assets.state_dir,
        shell_id,
        cwd,
        backend,
    )?;
    let session_init = msys_path(&session_init);
    let git_root = Path::new(program)
        .parent()
        .with_context(|| format!("resolve Git for Windows root from `{program}`"))?;
    let mintty = git_root.join("usr").join("bin").join("mintty.exe");
    if mintty.is_file() {
        Command::new(mintty)
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
) -> Result<()> {
    let mut args = vec![
        "-w".into(),
        "0".into(),
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
) -> Result<()> {
    let mut command = Command::new(program);
    command.current_dir(cwd);
    command.env("TOD_TERMINAL_BACKEND", backend);
    command.args(windows_launch_args(
        &assets.windows_init,
        shell_id,
        &assets.state_dir,
        cwd,
    ));
    command
        .spawn()
        .map(|_| ())
        .with_context(|| format!("spawn PowerShell `{program}` in {}", cwd.display()))
}

#[cfg(windows)]
fn spawn_windows_default(cwd: &Path, shell_id: &str, assets: &ShellInitAssets) -> Result<()> {
    if command_available("wt.exe") {
        return spawn_windows_terminal("wt.exe", cwd, shell_id, assets, "windows_terminal");
    }
    spawn_powershell("powershell.exe", cwd, shell_id, assets, "powershell")
}

#[cfg(target_os = "macos")]
fn spawn_macos_default(cwd: &Path, shell_id: &str, assets: &ShellInitAssets) -> Result<()> {
    if command_available("iTerm.app") || Path::new("/Applications/iTerm.app").exists() {
        return spawn_macos_terminal(cwd, shell_id, assets, "iterm");
    }
    spawn_macos_terminal(cwd, shell_id, assets, "macos_terminal")
}

#[cfg(target_os = "macos")]
fn spawn_macos_terminal(
    cwd: &Path,
    shell_id: &str,
    assets: &ShellInitAssets,
    backend: &str,
) -> Result<()> {
    let launch_cmd = posix_launch_command(
        &assets.posix_init,
        shell_id,
        &assets.state_dir,
        cwd,
        backend,
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
fn spawn_linux_default(cwd: &Path, shell_id: &str, assets: &ShellInitAssets) -> Result<()> {
    let cmd = posix_launch_command(
        &assets.posix_init,
        shell_id,
        &assets.state_dir,
        cwd,
        "posix",
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
    config_id: &str,
) -> Result<usize> {
    let shells = fleet.list_shells_for_config(config_id)?;
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
    let assets = ensure_shell_init_assets(paths)?;
    let shell_id = uuid::Uuid::new_v4().to_string();
    launch_shell_terminal(&cwd, &terminal, &shell_id, &assets)?;
    let state_dir = normalize_launch_path(&assets.state_dir);
    let state = wait_for_shell_state(&state_dir, &shell_id)?;
    let reconnect = reconnect_identity::record(state.pid);
    fleet.enqueue(FleetMutation::CreateShellSession {
        id: shell_id.clone(),
        agent_id: config_id.to_string(),
        reconnect,
    })?;
    fleet.writer().flush().context("persist shell session")?;
    Ok((shell_id, cwd))
}

/// Focus an existing shell terminal, or open a new one when the process is gone.
pub fn focus_shell_session(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    config_id: &str,
    node_id: &str,
    shell: &ShellSession,
) -> Result<PathBuf> {
    let agent = fleet
        .get_agent(config_id)?
        .with_context(|| format!("agent config {config_id} not found"))?;
    let cwd = resolve_agent_workspace(fleet, paths, settings, &agent, node_id)?;

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

    let (_, new_cwd) = open_shell_for_agent_config(fleet, paths, settings, config_id, node_id)?;
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
    fn posix_launch_command_quotes_paths() {
        let cmd = posix_launch_command(
            Path::new("/tmp/init.sh"),
            "shell-id",
            Path::new("/tmp/state"),
            Path::new("/tmp/work space"),
            "macos_terminal",
        );
        assert!(cmd.contains("'/tmp/work space'"));
        assert!(cmd.contains("TOD_TERMINAL_BACKEND=macos_terminal"));
    }

    #[cfg(windows)]
    #[test]
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
        let settings = TerminalSettings {
            program: Some(r"C:\app\dev\Git\git-bash.exe".into()),
            ..TerminalSettings::default()
        };

        launch_shell_terminal(&cwd, &settings, &shell_id, &assets).unwrap();
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
    fn open_shell_for_agent_config_registers_live_process() {
        use crate::fleet::repos::agent_config::NewAgentConfig;
        use crate::fleet::repos::task::FleetTask;
        use crate::fleet::store::FleetStore;
        use crate::fleet::test_util::{cleanup_fleet_root, temp_fleet_root};
        use crate::fleet::writer::FleetMutation;
        use crate::paths::{clear_data_root_override, set_data_root};
        use crate::settings::TodSettings;
        use reconnect_identity;

        let fleet_root = temp_fleet_root();
        let store = FleetStore::open(&fleet_root).unwrap();
        set_data_root(fleet_root.clone());
        let paths = crate::paths::TodPaths::discover().unwrap();

        let task_id = uuid::Uuid::new_v4().to_string();
        let config_id = format!("test-{}", uuid::Uuid::new_v4());
        let cwd = std::env::current_dir().unwrap();
        store
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(&task_id, "Shell test", "shell-test"),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store
            .enqueue(FleetMutation::InsertAgent {
                agent: NewAgentConfig {
                    id: config_id.clone(),
                    node_id: task_id.clone(),
                    env_type: "local".into(),
                    mode: "shell".into(),
                    work_directory: Some(cwd.display().to_string()),
                    use_worktree: false,
                },
            })
            .unwrap();
        store.writer().flush().unwrap();

        let settings = TodSettings::default();
        let (shell_id, resolved) =
            open_shell_for_agent_config(&store, &paths, &settings, &config_id, &task_id).unwrap();
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
