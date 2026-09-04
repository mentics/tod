//! Per-shell session state written by init scripts inside the terminal.

use crate::paths::TodPaths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellState {
    pub pid: u32,
    #[serde(default)]
    pub tty: Option<String>,
    #[serde(default)]
    pub hwnd: Option<u64>,
    #[serde(default)]
    pub backend: String,
}

pub fn shells_dir(paths: &TodPaths) -> PathBuf {
    paths.data_root().join("shells")
}

pub fn state_file_path(state_dir: &Path, shell_id: &str) -> PathBuf {
    state_dir.join(format!("{shell_id}.json"))
}

pub fn read_shell_state(path: &Path) -> Result<ShellState> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read shell state {}", path.display()))?;
    serde_json::from_str(&raw).context("parse shell state json")
}

pub fn remove_shell_state(paths: &TodPaths, shell_id: &str) {
    let path = state_file_path(&shells_dir(paths), shell_id);
    let _ = std::fs::remove_file(path);
}

pub fn write_shell_state(state_dir: &Path, shell_id: &str, pid: u32, backend: &str) -> Result<()> {
    let state = ShellState {
        pid,
        tty: None,
        hwnd: None,
        backend: backend.to_string(),
    };
    let path = state_file_path(state_dir, shell_id);
    let tmp = state_dir.join(format!("{shell_id}.json.tmp"));
    std::fs::write(&tmp, serde_json::to_string(&state)?)
        .with_context(|| format!("write shell state tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename shell state {}", path.display()))?;
    Ok(())
}

pub fn wait_for_shell_state(state_dir: &Path, shell_id: &str) -> Result<ShellState> {
    let path = state_file_path(state_dir, shell_id);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if path.is_file() {
            if let Ok(state) = read_shell_state(&path) {
                return Ok(state);
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for shell {} to register in {}",
                shell_id,
                state_dir.display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
