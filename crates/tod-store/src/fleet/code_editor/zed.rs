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

pub fn spawn_zed_url(url: &str, data_root: &Path) -> Result<()> {
    let env = zed_env(data_root)?;
    spawn_zed_with(&[url.to_string()], &env)
}

fn spawn_zed_with(args: &[String], env: &[(String, OsString)]) -> Result<()> {
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
        .map(|_| ())
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
