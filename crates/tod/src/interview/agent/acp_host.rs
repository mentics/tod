use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub use tod_store::AgentPlatform;

pub fn agent_platform_acp_host(platform: AgentPlatform) -> AcpHost {
    match platform {
        AgentPlatform::Cursor => AcpHost::Cursor,
        AgentPlatform::Claude => AcpHost::Claude,
    }
}

/// Configuration for an ACP-speaking agent CLI (Cursor, Claude, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpHost {
    Cursor,
    Claude,
}

impl AcpHost {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::Claude => "Claude",
        }
    }

    pub fn client_name(self) -> &'static str {
        match self {
            Self::Cursor => "tod-interview-ui",
            Self::Claude => "tod-interview-ui",
        }
    }

    pub fn auth_method_id(self) -> &'static str {
        match self {
            Self::Cursor => "cursor_login",
            Self::Claude => "claude_login",
        }
    }

    pub fn resolve_bin(self) -> Result<PathBuf> {
        match self {
            Self::Cursor => resolve_cursor_bin(),
            Self::Claude => resolve_claude_bin(),
        }
    }

    pub fn spawn_subcommand(self) -> &'static str {
        "acp"
    }
}

/// Resolve a CLI on `$PATH` using the user's login shell (macOS GUI apps often
/// inherit a minimal PATH that omits `~/.local/bin`).
#[cfg(unix)]
fn resolve_via_login_shell(name: &str) -> Option<PathBuf> {
    use std::process::Command;

    let output = Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v -- {name}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if path.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(path);
    candidate.is_file().then_some(candidate)
}

#[cfg(not(unix))]
fn resolve_via_login_shell(_name: &str) -> Option<PathBuf> {
    None
}

fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

fn resolve_cursor_bin() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("AGENT_BIN") {
        return Ok(PathBuf::from(path));
    }

    let mut candidates = Vec::new();
    if cfg!(windows) {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(&local_app_data)
                    .join("cursor-agent")
                    .join("agent.cmd"),
            );
            candidates.push(
                PathBuf::from(&local_app_data)
                    .join("cursor-agent")
                    .join("cursor-agent.cmd"),
            );
        }
    } else if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join(".local").join("bin").join("agent"));
        #[cfg(target_os = "macos")]
        {
            candidates.push(PathBuf::from("/opt/homebrew/bin/agent"));
            candidates.push(PathBuf::from("/usr/local/bin/agent"));
        }
    }

    if let Some(path) = first_existing(&candidates) {
        return Ok(path);
    }
    if let Some(path) = resolve_via_login_shell("agent") {
        return Ok(path);
    }

    bail!("Cursor agent CLI not found. Install from https://cursor.com/install or set AGENT_BIN.")
}

fn resolve_claude_bin() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("CLAUDE_BIN") {
        return Ok(PathBuf::from(path));
    }

    let mut candidates = Vec::new();
    if cfg!(windows) {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(&local_app_data)
                    .join("Programs")
                    .join("claude")
                    .join("claude.cmd"),
            );
        }
    } else if let Ok(home) = std::env::var("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".local")
                .join("bin")
                .join("claude"),
        );
        #[cfg(target_os = "macos")]
        {
            candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
            candidates.push(PathBuf::from("/usr/local/bin/claude"));
            candidates.push(
                PathBuf::from(home)
                    .join(".claude")
                    .join("local")
                    .join("bin")
                    .join("claude"),
            );
        }
    }

    if let Some(path) = first_existing(&candidates) {
        return Ok(path);
    }
    if let Some(path) = resolve_via_login_shell("claude") {
        return Ok(path);
    }

    bail!(
        "Claude agent CLI not found. Install with `curl -fsSL https://claude.ai/install.sh | bash` \
         or set CLAUDE_BIN to the full path (e.g. ~/.local/bin/claude)."
    )
}

/// Spawn `host acp` (or `cmd /C host.cmd acp` on Windows).
pub fn spawn_acp_process(host: AcpHost, agent_bin: &Path) -> Result<std::process::Child> {
    use std::process::{Command, Stdio};

    #[cfg(windows)]
    use std::os::windows::process::CommandExt;

    let subcommand = host.spawn_subcommand();
    let mut command = if agent_bin
        .extension()
        .is_some_and(|ext| ext == "cmd" || ext == "bat")
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", &agent_bin.to_string_lossy(), subcommand]);
        cmd
    } else if agent_bin
        .extension()
        .is_some_and(|ext| ext == "py" || ext == "pyw")
    {
        let mut cmd = Command::new("python");
        cmd.arg(agent_bin);
        cmd.arg(subcommand);
        cmd
    } else {
        let mut cmd = Command::new(agent_bin);
        cmd.arg(subcommand);
        cmd
    };

    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    command.spawn().with_context(|| {
        format!(
            "failed to spawn {} ACP ({})",
            host.label(),
            agent_bin.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_platform_default_is_claude() {
        assert_eq!(AgentPlatform::default(), AgentPlatform::Claude);
    }

    #[test]
    fn platform_maps_to_acp_host() {
        assert_eq!(
            agent_platform_acp_host(AgentPlatform::Cursor),
            AcpHost::Cursor
        );
        assert_eq!(
            agent_platform_acp_host(AgentPlatform::Claude),
            AcpHost::Claude
        );
    }
}
