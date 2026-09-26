//! Zed code editor plugin.

use crate::fleet::code_editor::CodeEditor;
use crate::fleet::terminal::path_util::normalize_launch_path;
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// CLI args for opening a workspace in Zed (focus-or-open via `--classic`).
pub fn zed_open_args(cwd: &Path) -> Vec<String> {
    vec!["--classic".into(), cwd.display().to_string()]
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
    let Some((path, position)) = file else {
        return spawn_zed_url(&folder_url, data_root);
    };
    // The file goes to a window whose remote project holds it, so the
    // folder's call has to have handed off to Zed first.
    let mut child = spawn_zed_child(&[folder_url], &zed_env(data_root)?)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait()?.is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    spawn_zed_url(&container_url(&user, host, path, position), data_root)
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
    Command::new(&bin)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn `{} {}`", bin.display(), args.join(" ")))
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
        // A running Zed holds the old copy open on Windows; it is replaced
        // the next time Zed is not running.
        if let Err(err) = std::fs::write(&target, &bytes) {
            if !target.is_file() {
                return Err(err).with_context(|| format!("install {}", target.display()));
            }
            tracing::warn!("keeping the older {}: {err}", target.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
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
