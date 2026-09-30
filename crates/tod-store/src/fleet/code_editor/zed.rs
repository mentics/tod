//! Zed code editor plugin.

use crate::fleet::code_editor::{CodeEditor, CodeLocation};
use crate::fleet::terminal::path_util::normalize_launch_path;
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
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
    let env = crate::paths::TodPaths::discover()
        .ok()
        .and_then(|paths| zed_env(paths.data_root()).ok())
        .unwrap_or_default();
    spawn_zed_with(&zed_open_args(&cwd), &env)
}

/// Opens `ssh://...` in Zed with the environment [`zed_env`] gives for `data_root`.
/// The Zed URL for `path` in sandbox `sandbox`, which tod's shim routes.
pub fn sandbox_url(sandbox: &str, path: &str) -> String {
    format!(
        "ssh://root@{}/{}",
        tod_sandbox::config::host_for(sandbox),
        path.trim_start_matches('/')
    )
}

/// Hosts ending in this are dev containers (`ssh://<user>@<container>.docker.tod/...`);
/// `tod-zed-shim` reaches them through `docker exec ... sshd -i`. Checked
/// there before the sandbox suffix (`.tod`), which it also ends in.
pub const CONTAINER_HOST_SUFFIX: &str = ".docker.tod";

/// The key pair Zed logs in to dev containers with, in [`SHIM_DIR`] (the
/// shim looks for it beside itself).
pub const CONTAINER_KEY_FILE: &str = "docker_ed25519";

/// The Zed URL for `path` (absolute) in dev container `container` as `user`,
/// at `line[:column]` when given.
pub fn container_url(user: &str, container: &str, path: &str, position: Option<(u32, Option<u32>)>) -> String {
    let mut url = format!(
        "ssh://{user}@{container}{CONTAINER_HOST_SUFFIX}/{}",
        path.trim_start_matches('/')
    );
    if let Some((line, column)) = position {
        url.push_str(&format!(":{line}"));
        if let Some(column) = column {
            url.push_str(&format!(":{column}"));
        }
    }
    url
}

/// No console window for a helper the app runs (Windows).
fn no_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// The public key line of tod's container key pair, generating the pair
/// with the host's `ssh-keygen` the first time.
pub fn ensure_container_key(data_root: &Path) -> Result<String> {
    let dir = data_root.join(SHIM_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let key = dir.join(CONTAINER_KEY_FILE);
    let public = dir.join(format!("{CONTAINER_KEY_FILE}.pub"));
    if !key.is_file() || !public.is_file() {
        let _ = std::fs::remove_file(&key);
        let _ = std::fs::remove_file(&public);
        let out = no_window(&mut Command::new("ssh-keygen"))
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "tod-zed", "-f"])
            .arg(&key)
            .stdin(Stdio::null())
            .output()
            .context("run ssh-keygen (is OpenSSH installed?)")?;
        if !out.status.success() {
            bail!("ssh-keygen: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        // Windows' ssh ignores a private key others can read, and the file
        // inherits the data root's permissions: keep only this user's.
        #[cfg(windows)]
        {
            let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
                (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
                (_, Ok(name)) => name,
                _ => bail!("USERNAME is not set; cannot protect {}", key.display()),
            };
            let out = no_window(&mut Command::new("icacls"))
                .arg(&key)
                .args(["/inheritance:r", "/grant:r", &format!("{user}:F")])
                .stdin(Stdio::null())
                .output()
                .context("run icacls")?;
            if !out.status.success() {
                let _ = std::fs::remove_file(&key);
                bail!(
                    "restrict {} to {user}: {}",
                    key.display(),
                    String::from_utf8_lossy(&out.stdout).trim()
                );
            }
        }
    }
    let line = std::fs::read_to_string(&public).with_context(|| format!("read {}", public.display()))?;
    Ok(line.trim().to_string())
}

/// Open `folder` in dev container `container` in Zed, then `file` (with its
/// position) when given: Zed takes one `ssh://` URL per call, and the file
/// then opens in the folder's window. Prepares the container's `sshd` first.
/// Talks to Docker: never call it on the UI thread.
pub fn open_in_container(
    data_root: &Path,
    container: &str,
    folder: &str,
    file: Option<(&str, Option<(u32, Option<u32>)>)>,
) -> Result<()> {
    let exec = tod_agent::devcontainer::ContainerExec::connect(container)?;
    let user = exec.user.clone().unwrap_or_else(|| "root".to_string());
    let key = ensure_container_key(data_root)?;
    tod_agent::devcontainer::prepare_sshd(&exec.id, &user, &key)?;
    // The name the user gave, so Zed's window titles and recent projects
    // stay stable across container ids.
    let host = if tod_agent::devcontainer::validate_container_ref(container).is_ok() {
        container
    } else {
        &exec.id
    };
    let folder_url = container_url(&user, host, folder, None);
    let env = zed_env(data_root)?;
    if env.is_empty() {
        anyhow::bail!(
            "tod-zed-shim is not installed next to tod, so Zed cannot reach dev container {container}; reinstall tod"
        );
    }
    let since = launch_time();
    let connection = format!("docker-{container}");
    let what = format!("dev container {container}");
    let Some((path, position)) = file else {
        spawn_zed_with(&[folder_url], &env)?;
        return require_connection(data_root, &connection, since, &what);
    };
    // The file goes to a window whose remote project holds it, so the
    // folder's call has to have handed off to Zed first.
    let before = zed_connections(&exec);
    let mut child = spawn_zed_child(&[folder_url], &env)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait()?.is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // A Zed that is not reaching the container through tod's shim will
    // never connect: say so rather than wait for it.
    require_connection(data_root, &connection, since, &what)?;
    // The CLI returns before Zed has connected, and a file sent before then
    // opens as a project of its own. A folder already open with Zed still
    // connected only comes to the front, with no new connection to wait for.
    let key = format!("{}:{folder}", exec.id);
    let mut opened = opened_folders().lock().unwrap_or_else(|e| e.into_inner());
    if !(opened.contains(&key) && before.is_some_and(|n| n > 0)) {
        wait_for_new_connection(&exec, before);
    }
    opened.insert(key);
    drop(opened);
    spawn_zed_with(&[container_url(&user, host, path, position)], &env)
}

/// Open `path` in cloud sandbox `sandbox` in Zed, and report an error when
/// no connection comes through tod's shim (see [`require_connection`]).
/// Waits up to [`CONNECTION_WAIT`]: never call it on the UI thread.
pub fn open_in_sandbox(data_root: &Path, sandbox: &str, path: &str) -> Result<()> {
    open_url_in_sandbox(data_root, sandbox, &sandbox_url(sandbox, path))
}

/// [`open_in_sandbox`] for a `url` already made from [`sandbox_url`] (with a
/// line and column, say).
pub fn open_url_in_sandbox(data_root: &Path, sandbox: &str, url: &str) -> Result<()> {
    let env = zed_env(data_root)?;
    if env.is_empty() {
        anyhow::bail!("tod-zed-shim is not installed next to tod, so Zed cannot reach sandbox {sandbox}; reinstall tod");
    }
    let since = launch_time();
    spawn_zed_with(&[url.to_string()], &env)?;
    require_connection(data_root, &format!("sandbox-{sandbox}"), since, &format!("sandbox {sandbox}"))
}

/// Where `tod-zed-shim` records the connections it carries, under
/// [`SHIM_DIR`]: `connections/<key>/<pid>`, `<key>` being `docker-<container>`
/// or `sandbox-<name>`. The shim holds a lock on its file while it runs. Must
/// match the shim's `CONNECTIONS_DIR`.
pub const CONNECTIONS_DIR: &str = "connections";

/// How long to wait for Zed to connect through the shim after tod asks it
/// to open a remote folder.
pub const CONNECTION_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// A little before now, so a marker written in the same clock tick (or on a
/// file system with coarse timestamps) still counts as new.
fn launch_time() -> std::time::SystemTime {
    std::time::SystemTime::now() - std::time::Duration::from_secs(2)
}

/// Waits for Zed to reach the host through tod's shim; an error means it
/// did not. Windows Zed is single-instance: a Zed that was already running,
/// not started by tod, gets tod's request but has no shim on its `PATH`, so
/// its own `ssh` fails on the `.tod` host and nothing else says so.
///
/// A Zed tod started that is already connected to this host may reuse that
/// connection and run no new `ssh`; its shim processes (the connection's
/// master and proxy) are still running and hold their markers' locks, which
/// counts as connected.
fn require_connection(data_root: &Path, key: &str, since: std::time::SystemTime, what: &str) -> Result<()> {
    let dir = data_root.join(SHIM_DIR).join(CONNECTIONS_DIR).join(key);
    if wait_for_connection(&dir, since, CONNECTION_WAIT, std::time::Duration::from_millis(250)) {
        return Ok(());
    }
    anyhow::bail!(
        "Zed was already open without tod's connection helper, so it could not reach {what}. Quit Zed and open again."
    )
}

/// Polls [`connection_seen`] until it holds or `timeout` passes.
pub fn wait_for_connection(
    dir: &Path,
    since: std::time::SystemTime,
    timeout: std::time::Duration,
    poll: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if connection_seen(dir, since) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Whether a shim has carried a connection for this host since `since` (a
/// marker written then), or carries one now (a marker it still locks).
/// Unlocked markers from before are leftovers and are removed.
pub fn connection_seen(dir: &Path, since: std::time::SystemTime) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut seen = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t >= since) {
            seen = true;
            continue;
        }
        let Ok(file) = std::fs::OpenOptions::new().read(true).open(&path) else {
            continue;
        };
        match file.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => seen = true,
            Err(std::fs::TryLockError::Error(_)) => {}
            Ok(()) => {
                drop(file);
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    seen
}

/// Container folders tod has opened in Zed during this run.
fn opened_folders() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static OPENED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    OPENED.get_or_init(Default::default)
}

/// How many Zed clients are connected to the container: its remote server
/// runs one `proxy` per connection. `None` when it cannot tell.
fn zed_connections(exec: &tod_agent::devcontainer::ContainerExec) -> Option<usize> {
    let script = "for f in /proc/[0-9]*/cmdline; do tr '\\0' ' ' < \"$f\" 2>/dev/null; echo; done \
                  | grep -c '[z]ed-remote-server.*proxy'";
    let out = exec.output("/", "sh", &["-c", script]).ok()?;
    // grep -c exits 1 when it counts none.
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Wait until Zed has a connection to the container it did not have
/// `before`: long when it has none (the first connect installs Zed's
/// server), briefly otherwise.
fn wait_for_new_connection(exec: &tod_agent::devcontainer::ContainerExec, before: Option<usize>) {
    use std::time::{Duration, Instant};
    let Some(before) = before else {
        std::thread::sleep(Duration::from_secs(3));
        return;
    };
    let limit = if before == 0 { 120 } else { 10 };
    let deadline = Instant::now() + Duration::from_secs(limit);
    while Instant::now() < deadline {
        if zed_connections(exec).is_some_and(|now| now > before) {
            // Let the new window take the project before the file arrives.
            std::thread::sleep(Duration::from_secs(1));
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn spawn_zed_url(url: &str, data_root: &Path) -> Result<()> {
    let env = zed_env(data_root)?;
    spawn_zed_with(&[url.to_string()], &env)
}

fn spawn_zed_with(args: &[String], env: &[(String, OsString)]) -> Result<()> {
    spawn_zed_child(args, env).map(|_| ())
}

fn spawn_zed_child(args: &[String], env: &[(String, OsString)]) -> Result<std::process::Child> {
    let bin = resolve_zed_bin().ok_or_else(|| {
        anyhow::anyhow!(
            "Zed CLI not found. Install Zed and ensure `zed` is on PATH \
             (macOS: Command Palette → \"cli: install cli binary\"; \
             Windows: typically %LOCALAPPDATA%\\Programs\\Zed\\bin)."
        )
    })?;
    allow_foreground();
    Command::new(&bin)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn `{} {}`", bin.display(), args.join(" ")))
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

/// Where Zed's `ssh`, `scp`, and `sftp` stand-ins live, under the data root.
pub const SHIM_DIR: &str = "zed-shim";

fn exe_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

/// The environment for a Zed that tod starts.
///
/// tod is assumed to be the only thing that starts Zed. Its `ssh` is
/// `tod-zed-shim` (installed beside this executable), copied into
/// `<data_root>/zed-shim/` as `ssh`, `scp`, and `sftp` and put first on Zed's
/// PATH: hosts named `<sandbox>.tod` go to the sandbox's relay, every other
/// host to the real `ssh`. The shim finds `tod-sandbox` and the data root
/// through `TOD_SANDBOX_BIN` and `TOD_DATA_ROOT`. Without an installed shim
/// the environment is empty and Zed starts as it always did.
pub fn zed_env(data_root: &Path) -> Result<Vec<(String, OsString)>> {
    let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else {
        return Ok(Vec::new());
    };
    let shim = exe_dir.join(exe_name("tod-zed-shim"));
    if !shim.is_file() {
        return Ok(Vec::new());
    }
    let dir = data_root.join(SHIM_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let bytes = std::fs::read(&shim).with_context(|| format!("read {}", shim.display()))?;
    for name in ["ssh", "scp", "sftp"] {
        let target = dir.join(exe_name(name));
        if std::fs::read(&target).is_ok_and(|current| current == bytes) {
            continue;
        }
        // Written beside it and renamed over it, never rewritten in place:
        // macOS caches a binary's code signature by file, and kills (SIGKILL)
        // a signed binary whose bytes changed under it. A running Zed holds
        // the old copy open on Windows; it is replaced the next time Zed is
        // not running.
        let staged = dir.join(format!(".{}.new", exe_name(name)));
        let installed = std::fs::write(&staged, &bytes).and_then(|()| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
            }
            std::fs::rename(&staged, &target)
        });
        if let Err(err) = installed {
            let _ = std::fs::remove_file(&staged);
            if !target.is_file() {
                return Err(err).with_context(|| format!("install {}", target.display()));
            }
            tracing::warn!("keeping the older {}: {err}", target.display());
        }
    }
    let mut path = OsString::from(dir.as_os_str());
    if let Some(old) = std::env::var_os("PATH") {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(old);
    }
    let mut env = vec![
        ("PATH".to_string(), path),
        ("TOD_DATA_ROOT".to_string(), data_root.as_os_str().to_os_string()),
    ];
    let sandbox_bin = exe_dir.join(exe_name("tod-sandbox"));
    if sandbox_bin.is_file() {
        env.push(("TOD_SANDBOX_BIN".to_string(), sandbox_bin.into_os_string()));
    }
    Ok(env)
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

    fn open_location(&self, root: Option<&Path>, file: &Path, location: &CodeLocation) -> Result<()> {
        let env = crate::paths::TodPaths::discover()
            .ok()
            .and_then(|paths| zed_env(paths.data_root()).ok())
            .unwrap_or_default();
        spawn_zed_with(&zed_location_args(root, file, location), &env)
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
    fn container_urls_name_the_container_host_and_position() {
        assert_eq!(
            container_url("vscode", "my-dev", "/workspaces/app", None),
            "ssh://vscode@my-dev.docker.tod/workspaces/app"
        );
        assert_eq!(
            container_url("root", "c1", "/w/src/main.rs", Some((12, Some(4)))),
            "ssh://root@c1.docker.tod/w/src/main.rs:12:4"
        );
        assert_eq!(
            container_url("root", "c1", "/w/a.rs", Some((7, None))),
            "ssh://root@c1.docker.tod/w/a.rs:7"
        );
        // The sandbox suffix also matches; the shim checks this one first.
        assert!(CONTAINER_HOST_SUFFIX.ends_with(tod_sandbox::config::HOST_SUFFIX));
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("tod-zed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Scratch(dir)
    }

    fn short() -> (std::time::Duration, std::time::Duration) {
        (std::time::Duration::from_millis(300), std::time::Duration::from_millis(20))
    }

    #[test]
    fn no_connection_times_out() {
        let root = scratch("conn-none");
        let (timeout, poll) = short();
        let since = std::time::SystemTime::now();
        assert!(!wait_for_connection(&root.path().join("docker-c"), since, timeout, poll));
        std::fs::create_dir_all(root.path()).unwrap();
        assert!(!wait_for_connection(root.path(), since, timeout, poll));
    }

    #[test]
    fn a_marker_written_after_launch_counts() {
        let root = scratch("conn-new");
        let dir = root.path().join("docker-c");
        let since = launch_time();
        let writer = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(50));
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("123"), b"").unwrap();
            })
        };
        let (_, poll) = short();
        assert!(wait_for_connection(&dir, since, std::time::Duration::from_secs(5), poll));
        writer.join().unwrap();
    }

    #[test]
    fn a_held_old_marker_counts_and_a_stale_one_is_removed() {
        let root = scratch("conn-old");
        let dir = root.path().join("docker-c");
        std::fs::create_dir_all(&dir).unwrap();
        let held = std::fs::File::create(dir.join("1")).unwrap();
        held.lock().unwrap();
        std::fs::write(dir.join("2"), b"").unwrap();
        // Both markers predate this launch.
        let since = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        let (timeout, poll) = short();
        assert!(wait_for_connection(&dir, since, timeout, poll));
        assert!(!dir.join("2").exists(), "the unlocked leftover is removed");
        drop(held);
        assert!(!wait_for_connection(&dir, since, timeout, poll));
        assert!(!dir.join("1").exists());
    }

    /// Needs `ssh-keygen` on PATH; skipped without it.
    #[test]
    fn container_key_is_generated_once() {
        if Command::new("ssh-keygen").arg("-?").output().is_err() {
            return;
        }
        let root = scratch("key");
        let first = ensure_container_key(root.path()).unwrap();
        assert!(first.starts_with("ssh-ed25519 "), "{first}");
        assert_eq!(ensure_container_key(root.path()).unwrap(), first);
        assert!(root.path().join(SHIM_DIR).join(CONTAINER_KEY_FILE).is_file());
    }

    /// Needs `TOD_TEST_DEV_CONTAINER`: a running container with `sshd`.
    /// Prepares it the way opening it in Zed does (without opening Zed).
    #[test]
    fn prepares_a_real_container_for_zed() {
        let Ok(container) = std::env::var("TOD_TEST_DEV_CONTAINER") else {
            return;
        };
        let root = scratch("prep");
        let key = ensure_container_key(root.path()).unwrap();
        let exec = tod_agent::devcontainer::ContainerExec::connect(&container).unwrap();
        let user = exec.user.clone().unwrap_or_else(|| "root".into());
        tod_agent::devcontainer::prepare_sshd(&exec.id, &user, &key).unwrap();
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
