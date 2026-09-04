//! Open an agent workspace directory in the Zed editor.

use crate::fleet::terminal::path_util::normalize_launch_path;
use crate::fleet::{FleetStore, resolve_agent_workspace};
use crate::paths::TodPaths;
use crate::settings::TodSettings;
use anyhow::{Context, Result, bail};
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
    let bin = resolve_zed_bin().ok_or_else(|| {
        anyhow::anyhow!(
            "Zed CLI not found. Install Zed and ensure `zed` is on PATH \
             (macOS: Command Palette → \"cli: install cli binary\"; \
             Windows: typically %LOCALAPPDATA%\\Programs\\Zed\\bin)."
        )
    })?;
    let args = zed_open_args(&cwd);
    Command::new(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn `{} {}`", bin.display(), args.join(" ")))
        .map(|_| ())
}

/// Resolve the agent config workspace (provisioning a worktree if needed) and open it in Zed.
pub fn open_zed_for_agent_config(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    config_id: &str,
    _node_id: &str,
) -> Result<PathBuf> {
    let agent = fleet
        .get_agent(config_id)?
        .with_context(|| format!("agent config {config_id} not found"))?;
    let cwd = resolve_agent_workspace(fleet, paths, settings, &agent)?;
    spawn_zed(&cwd)?;
    Ok(normalize_launch_path(&cwd))
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
