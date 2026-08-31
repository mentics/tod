use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Interview agent platform — persisted in `tod.yml` and shown in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPlatform {
    Cursor,
    #[serde(alias = "anthropic")]
    Claude,
}

impl Default for AgentPlatform {
    fn default() -> Self {
        Self::Claude
    }
}

impl AgentPlatform {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::Claude => "Claude",
        }
    }

    pub fn acp_host(self) -> AcpHost {
        match self {
            Self::Cursor => AcpHost::Cursor,
            Self::Claude => AcpHost::Claude,
        }
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
    }
    candidates.push(PathBuf::from("agent"));

    for candidate in candidates {
        if candidate == PathBuf::from("agent") || candidate.exists() {
            return Ok(candidate);
        }
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
        candidates.push(PathBuf::from(home).join(".local").join("bin").join("claude"));
    }
    candidates.push(PathBuf::from("claude"));

    for candidate in candidates {
        if candidate == PathBuf::from("claude") || candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!("Claude agent CLI not found. Install the Claude CLI or set CLAUDE_BIN.")
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
    } else if agent_bin.extension().is_some_and(|ext| ext == "py" || ext == "pyw") {
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

    command
        .spawn()
        .with_context(|| format!("failed to spawn {} ACP ({})", host.label(), agent_bin.display()))
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
        assert_eq!(AgentPlatform::Cursor.acp_host(), AcpHost::Cursor);
        assert_eq!(AgentPlatform::Claude.acp_host(), AcpHost::Claude);
    }
}
