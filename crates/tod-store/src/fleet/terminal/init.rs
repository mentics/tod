//! Bootstrap scripts installed under the data root and sourced inside agent shells.

use crate::paths::TodPaths;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const POSIX_INIT_SCRIPT: &str = include_str!("scripts/tod-shell-init.sh");
const WINDOWS_INIT_SCRIPT: &str = include_str!("scripts/tod-shell-init.ps1");

pub struct ShellInitAssets {
    pub state_dir: PathBuf,
    pub posix_init: PathBuf,
    #[cfg(windows)]
    pub windows_init: PathBuf,
}

pub fn ensure_shell_init_assets(paths: &TodPaths) -> Result<ShellInitAssets> {
    let state_dir = paths.data_root().join("shells");
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("create shells dir {}", state_dir.display()))?;

    let posix_init = state_dir.join("tod-shell-init.sh");
    std::fs::write(&posix_init, POSIX_INIT_SCRIPT)
        .with_context(|| format!("write {}", posix_init.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&posix_init, std::fs::Permissions::from_mode(0o755))?;
    }

    #[cfg(windows)]
    {
        let windows_init = state_dir.join("tod-shell-init.ps1");
        std::fs::write(&windows_init, WINDOWS_INIT_SCRIPT)
            .with_context(|| format!("write {}", windows_init.display()))?;
        return Ok(ShellInitAssets {
            state_dir,
            posix_init,
            windows_init,
        });
    }

    #[cfg(not(windows))]
    Ok(ShellInitAssets {
        state_dir,
        posix_init,
    })
}

pub fn sh_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if !value.contains('\'') {
        return format!("'{value}'");
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn write_session_init_script(
    init_script: &Path,
    state_dir: &Path,
    shell_id: &str,
    cwd: &Path,
    backend: &str,
) -> Result<PathBuf> {
    let path = state_dir.join(format!("{shell_id}-init.sh"));
    let content = format!(
        "#!/usr/bin/env bash\nexport TOD_TERMINAL_BACKEND={backend}\nsource {} {} {} {}\n",
        sh_quote(&msys_path(init_script)),
        sh_quote(shell_id),
        sh_quote(&msys_path(state_dir)),
        sh_quote(&msys_path(cwd)),
    );
    std::fs::write(&path, content)
        .with_context(|| format!("write session init {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(path)
}

/// MSYS-friendly path (`C:/data/...`) for Git Bash / mintty arguments.
pub fn msys_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
pub fn posix_launch_command(
    init_script: &Path,
    shell_id: &str,
    state_dir: &Path,
    cwd: &Path,
    backend: &str,
) -> String {
    format!(
        "export TOD_TERMINAL_BACKEND={backend}; source {} {} {} {}",
        sh_quote(&init_script.to_string_lossy()),
        sh_quote(shell_id),
        sh_quote(&state_dir.to_string_lossy()),
        sh_quote(&cwd.to_string_lossy()),
    )
}

#[cfg(windows)]
pub fn windows_launch_args(
    init_script: &Path,
    shell_id: &str,
    state_dir: &Path,
    cwd: &Path,
) -> Vec<String> {
    vec![
        "-NoExit".into(),
        "-NoLogo".into(),
        "-File".into(),
        init_script.display().to_string(),
        "-TodShellId".into(),
        shell_id.to_string(),
        "-TodStateDir".into(),
        state_dir.display().to_string(),
        "-TodCwd".into(),
        cwd.display().to_string(),
    ]
}
