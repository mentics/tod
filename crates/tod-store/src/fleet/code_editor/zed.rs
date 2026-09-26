//! Zed code editor plugin.

use crate::fleet::code_editor::{CodeEditor, CodeLocation, RemoteHost};
use crate::fleet::terminal::path_util::normalize_launch_path;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// CLI args for opening a workspace in Zed (focus-or-open via `--classic`).
pub fn zed_open_args(cwd: &Path) -> Vec<String> {
    vec!["--classic".into(), cwd.display().to_string()]
}

/// CLI args for opening `file` at `location`'s position. With `root`, the
/// file opens in that workspace: Zed reuses a window that already has it.
pub fn zed_location_args(root: Option<&Path>, file: &Path, location: &CodeLocation) -> Vec<String> {
    let mut args = vec!["--classic".to_string()];
    if let Some(root) = root {
        args.push(root.display().to_string());
    }
    args.push(location.with_position(&file.display().to_string()));
    args
}

/// `ssh://user@host/path`, the form Zed opens a remote path in.
pub fn zed_ssh_url(host: &RemoteHost, path: &str) -> String {
    format!("ssh://{}@{}{}", host.user, host.alias, path)
}

/// CLI calls that open `dir` on `host`, then `file` at its position in it.
/// Each call takes one `ssh://` URL (a second fails as "cannot open both
/// local and ssh paths"), so the file is a call of its own: it lands in the
/// window the first opened.
pub fn zed_remote_calls(
    host: &RemoteHost,
    dir: &str,
    file: Option<(&str, &CodeLocation)>,
) -> Vec<Vec<String>> {
    let mut calls = vec![vec!["--classic".to_string(), zed_ssh_url(host, dir)]];
    if let Some((file, location)) = file {
        calls.push(vec![
            "--classic".to_string(),
            zed_ssh_url(host, &location.with_position(file)),
        ]);
    }
    calls
}

/// Candidate binary names to try on PATH (order matters).
pub fn zed_bin_candidates() -> &'static [&'static str] {
    &["zed", "zeditor"]
}

fn command_on_path(name: &str) -> bool {
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

fn known_install_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local).join("Programs").join("Zed");
            out.push(base.join("bin").join("zed.exe"));
            out.push(base.join("bin").join("zed"));
            out.push(base.join("Zed.exe"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from("/usr/local/bin/zed"));
        out.push(PathBuf::from("/Applications/Zed.app/Contents/MacOS/cli"));
    }
    out
}

/// Resolve the Zed CLI binary (PATH first, then known install locations).
pub fn resolve_zed_bin() -> Option<PathBuf> {
    for name in zed_bin_candidates() {
        if command_on_path(name) {
            return Some(PathBuf::from(name));
        }
    }
    known_install_candidates().into_iter().find(|p| p.is_file())
}

/// Spawn Zed for `cwd` without waiting (hands off to the running app when present).
pub fn spawn_zed(cwd: &Path) -> Result<()> {
    let cwd = normalize_launch_path(cwd);
    if !cwd.is_dir() {
        bail!("workspace directory does not exist: {}", cwd.display());
    }
    run_zed(&zed_open_args(&cwd))
}

/// Run the Zed CLI with `args` without waiting for it.
fn run_zed(args: &[String]) -> Result<()> {
    zed_command(args)?
        .spawn()
        .with_context(|| format!("spawn `zed {}`", args.join(" ")))
        .map(|_| ())
}

/// How long a remote open may take before tod stops waiting for it: the first
/// connection to a container installs Zed's server there.
const REMOTE_OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Run the Zed CLI with `args` and wait until it hands the request over (or
/// [`REMOTE_OPEN_WAIT`] passes).
fn run_zed_waiting(args: &[String]) -> Result<()> {
    let mut child = zed_command(args)?
        .spawn()
        .with_context(|| format!("spawn `zed {}`", args.join(" ")))?;
    let started = std::time::Instant::now();
    while started.elapsed() < REMOTE_OPEN_WAIT {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("`zed {}` failed ({status})", args.join(" "));
            }
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

fn zed_command(args: &[String]) -> Result<Command> {
    let bin = resolve_zed_bin().ok_or_else(|| {
        anyhow::anyhow!(
            "Zed CLI not found. Install Zed and ensure `zed` is on PATH \
             (macOS: Command Palette → \"cli: install cli binary\"; \
             Windows: typically %LOCALAPPDATA%\\Programs\\Zed\\bin)."
        )
    })?;
    allow_foreground();
    let mut command = Command::new(&bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

/// Let the Zed window come to the front. The CLI hands the request to the
/// running Zed, a process Windows would otherwise not let take the foreground
/// from tod, so its window would only flash in the taskbar.
fn allow_foreground() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
        // Fails harmlessly when tod is not in the foreground itself.
        let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    }
}

/// The Zed [`CodeEditor`] plugin.
pub struct ZedEditor;

impl CodeEditor for ZedEditor {
    fn id(&self) -> &'static str {
        "zed"
    }

    fn label(&self) -> &'static str {
        "Zed"
    }

    fn is_available(&self) -> bool {
        resolve_zed_bin().is_some()
    }

    fn open(&self, dir: &Path) -> Result<()> {
        spawn_zed(dir)
    }

    fn open_location(
        &self,
        root: Option<&Path>,
        file: &Path,
        location: &CodeLocation,
    ) -> Result<()> {
        run_zed(&zed_location_args(root, file, location))
    }

    fn open_remote(
        &self,
        host: &RemoteHost,
        dir: &str,
        file: Option<(&str, &CodeLocation)>,
    ) -> Result<()> {
        let calls = zed_remote_calls(host, dir, file);
        let (folder, files) = calls.split_first().context("no directory to open")?;
        let key = format!("{}@{}:{dir}", host.user, host.alias);
        let opened = |key: &str| opened_dirs().lock().expect("opened dirs").contains(key);
        if !files.is_empty() && opened(&key) {
            // Its window is most likely still open: the file lands in it.
            return files.iter().try_for_each(|args| run_zed_waiting(args));
        }
        let before = zed_connections(host);
        run_zed_waiting(folder)?;
        opened_dirs().lock().expect("opened dirs").insert(key);
        if !files.is_empty() {
            wait_for_new_connection(host, before);
        }
        files.iter().try_for_each(|args| run_zed_waiting(args))
    }
}

/// Remote directories opened in Zed since tod started.
fn opened_dirs() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static OPENED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    OPENED.get_or_init(Default::default)
}

/// How many clients Zed's remote server on `host` is serving: it runs one
/// `proxy` per connection. Read from `/proc`, which needs no `ps` in the
/// image. `None` when it cannot tell.
fn zed_connections(host: &RemoteHost) -> Option<usize> {
    const COUNT: &str = "for f in /proc/[0-9]*/cmdline; do tr '\\0' ' ' < \"$f\" 2>/dev/null; echo; done \
                         | grep -c '[z]ed-remote-server.*proxy'";
    let mut command = Command::new("ssh");
    command
        .args(["-o", "BatchMode=yes", &host.alias, COUNT])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let out = command.output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Wait until Zed's connection for a newly opened directory is up, so a file
/// sent next lands in that window instead of opening a project of its own.
/// The first connection to a host installs Zed's server there and may take
/// long; with a server already serving, a new client shows in a second or
/// two, or not at all when Zed only focused a window it had.
fn wait_for_new_connection(host: &RemoteHost, before: Option<usize>) {
    let Some(before) = before else {
        std::thread::sleep(std::time::Duration::from_secs(3));
        return;
    };
    let limit = if before == 0 {
        REMOTE_OPEN_WAIT
    } else {
        std::time::Duration::from_secs(10)
    };
    let started = std::time::Instant::now();
    while started.elapsed() < limit {
        if zed_connections(host).is_some_and(|now| now > before) {
            // The window registers its project just after connecting.
            std::thread::sleep(std::time::Duration::from_secs(1));
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_args_use_classic_and_path() {
        let cwd = PathBuf::from("/tmp/workspace");
        assert_eq!(
            zed_open_args(&cwd),
            vec!["--classic".to_string(), "/tmp/workspace".to_string()]
        );
    }

    #[test]
    fn location_args_open_the_file_in_its_workspace() {
        let location = CodeLocation::parse("src/main.rs:6:4").unwrap();
        let root = PathBuf::from("/w/demo");
        let file = root.join("src/main.rs");
        assert_eq!(
            zed_location_args(Some(&root), &file, &location),
            vec![
                "--classic".to_string(),
                root.display().to_string(),
                format!("{}:6:4", file.display()),
            ]
        );
        let bare = CodeLocation::parse("/w/x.rs").unwrap();
        assert_eq!(
            zed_location_args(None, Path::new("/w/x.rs"), &bare),
            vec!["--classic".to_string(), "/w/x.rs".to_string()]
        );
    }

    #[test]
    fn remote_calls_open_the_directory_then_the_file() {
        let host = RemoteHost {
            alias: "tod-dev".into(),
            user: "vscode".into(),
        };
        let location = CodeLocation::parse("src/a.rs:3:2").unwrap();
        assert_eq!(
            zed_remote_calls(&host, "/w/p", Some(("/w/p/src/a.rs", &location))),
            vec![
                vec![
                    "--classic".to_string(),
                    "ssh://vscode@tod-dev/w/p".to_string()
                ],
                vec![
                    "--classic".to_string(),
                    "ssh://vscode@tod-dev/w/p/src/a.rs:3:2".to_string()
                ],
            ]
        );
        assert_eq!(zed_remote_calls(&host, "/w/p", None).len(), 1);
    }

    #[test]
    fn candidates_prefer_zed() {
        assert_eq!(zed_bin_candidates()[0], "zed");
        assert!(zed_bin_candidates().contains(&"zeditor"));
    }

    #[test]
    fn spawn_rejects_missing_directory() {
        let missing = PathBuf::from("/definitely/does/not/exist/tod-zed-test");
        let err = spawn_zed(&missing).unwrap_err();
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn resolve_finds_windows_install_or_path() {
        // Smoke: either PATH or known install; must not panic.
        let _ = resolve_zed_bin();
    }
}
