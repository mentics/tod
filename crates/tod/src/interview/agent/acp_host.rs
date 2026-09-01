use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub use tod_store::AgentPlatform;

#[cfg(test)]
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
            Self::Claude => resolve_claude_acp_bin(),
        }
    }

    /// Whether spawn should append the `acp` subcommand (Cursor / native Claude) or run
    /// the binary directly (standalone ACP adapters such as `claude-code-acp`).
    pub fn uses_acp_subcommand(self, agent_bin: &Path) -> bool {
        match self {
            Self::Cursor => true,
            Self::Claude => !is_standalone_acp_server(agent_bin),
        }
    }
}

/// Standalone ACP bridges (e.g. Zed's `claude-code-acp`) speak ACP on stdio directly
/// and rely on the user's existing `claude` CLI login — they do not implement
/// `authenticate` with `claude_login`.
pub fn is_standalone_acp_server(agent_bin: &Path) -> bool {
    let Some(name) = agent_bin.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let stem = name
        .strip_suffix(".cmd")
        .or_else(|| name.strip_suffix(".exe"))
        .unwrap_or(name);
    stem.contains("claude-code-acp") || stem.contains("claude-code-cli-acp")
}

/// True when `claude acp` is a supported subcommand (not present on current Claude Code CLI).
fn claude_supports_native_acp(claude_bin: &Path) -> bool {
    use std::process::{Command, Stdio};

    Command::new(claude_bin)
        .args(["acp", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn claude_acp_adapter_candidates(home: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(home)
            .join(".local")
            .join("bin")
            .join("claude-code-acp"),
    ];
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            candidates.push(
                PathBuf::from(&appdata)
                    .join("npm")
                    .join("claude-code-acp.cmd"),
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from("/opt/homebrew/bin/claude-code-acp"));
        candidates.push(PathBuf::from("/usr/local/bin/claude-code-acp"));
    }
    candidates
}

fn claude_cli_candidates(home: &str) -> Vec<PathBuf> {
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
    } else {
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
    candidates
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

fn resolve_claude_acp_bin() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("CLAUDE_ACP_BIN") {
        return Ok(PathBuf::from(path));
    }

    if let Ok(path) = std::env::var("CLAUDE_BIN") {
        let candidate = PathBuf::from(&path);
        if candidate.is_file()
            && (is_standalone_acp_server(&candidate) || claude_supports_native_acp(&candidate))
        {
            return Ok(candidate);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        if let Some(path) = first_existing(&claude_acp_adapter_candidates(&home)) {
            return Ok(path);
        }
    }
    if let Some(path) = resolve_via_login_shell("claude-code-acp") {
        return Ok(path);
    }

    if let Ok(home) = std::env::var("HOME") {
        for candidate in claude_cli_candidates(&home) {
            if candidate.is_file() && claude_supports_native_acp(&candidate) {
                return Ok(candidate);
            }
        }
    }
    if let Some(claude) = resolve_via_login_shell("claude") {
        if claude_supports_native_acp(&claude) {
            return Ok(claude);
        }
    }

    bail!(
        "Claude ACP adapter not found. The `claude` CLI does not include an `acp` subcommand — \
         install Zed's adapter:\n  \
         npm install -g @zed-industries/claude-code-acp\n\
         Then ensure `claude-code-acp` is on PATH, or set CLAUDE_ACP_BIN to its full path."
    )
}

/// Spawn an ACP server process (Cursor/Claude `… acp`, or a standalone adapter).
pub fn spawn_acp_process(host: AcpHost, agent_bin: &Path) -> Result<std::process::Child> {
    use std::process::{Command, Stdio};

    let use_subcommand = host.uses_acp_subcommand(agent_bin);
    let subcommand = "acp";
    let mut command = if agent_bin
        .extension()
        .is_some_and(|ext| ext == "cmd" || ext == "bat")
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(agent_bin);
        if use_subcommand {
            cmd.arg(subcommand);
        }
        cmd
    } else if agent_bin
        .extension()
        .is_some_and(|ext| ext == "py" || ext == "pyw")
    {
        let mut cmd = Command::new("python");
        cmd.arg(agent_bin);
        if use_subcommand {
            cmd.arg(subcommand);
        }
        cmd
    } else {
        let mut cmd = Command::new(agent_bin);
        if use_subcommand {
            cmd.arg(subcommand);
        }
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
    use std::path::Path;

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

    #[test]
    fn standalone_acp_server_detection() {
        assert!(is_standalone_acp_server(Path::new(
            "/usr/local/bin/claude-code-acp"
        )));
        assert!(is_standalone_acp_server(Path::new(
            r"C:\Users\me\AppData\Roaming\npm\claude-code-acp.cmd"
        )));
        assert!(!is_standalone_acp_server(Path::new(
            "/usr/local/bin/claude"
        )));
        assert!(AcpHost::Claude.uses_acp_subcommand(Path::new("/usr/local/bin/claude-code-acp")));
        assert!(AcpHost::Cursor.uses_acp_subcommand(Path::new("/usr/local/bin/agent")));
    }
}
