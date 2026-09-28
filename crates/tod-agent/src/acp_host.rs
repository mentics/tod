use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[allow(unused_imports)]
pub use crate::platform::AgentPlatform;

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
    pub fn platform(self) -> AgentPlatform {
        match self {
            Self::Cursor => AgentPlatform::Cursor,
            Self::Claude => AgentPlatform::Claude,
        }
    }

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

    /// Whether spawn appends the `acp` subcommand (Cursor's CLI) or runs the
    /// binary directly (Claude's adapter, `claude-agent-acp`, speaks ACP on
    /// stdio).
    pub fn uses_acp_subcommand(self, _agent_bin: &Path) -> bool {
        match self {
            Self::Cursor => true,
            Self::Claude => false,
        }
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

/// Claude's ACP adapter, `claude-agent-acp` (see [`crate::claude_adapter`]).
/// Without it tod does not run Claude: the error says how to install it.
fn resolve_claude_acp_bin() -> Result<PathBuf> {
    crate::claude_adapter::find()
        .map(|adapter| adapter.bin)
        .ok_or_else(|| anyhow::anyhow!(crate::claude_adapter::not_installed_message()))
}

/// Locate `bash.exe` from a Git for Windows install, for `CLAUDE_CODE_GIT_BASH_PATH`.
///
/// The `claude` CLI's Bash tool needs Git Bash on Windows and only finds it via
/// that env var or `PATH`; a GUI-launched `tod` doesn't always inherit a `PATH`
/// with Git's `bin`/`usr\bin` on it even when a terminal's does, so we resolve
/// it ourselves rather than relying on inheritance.
#[cfg(windows)]
fn resolve_git_bash_path() -> Option<PathBuf> {
    use std::process::Command;

    let is_bash = |p: &PathBuf| p.is_file();

    let mut candidates = Vec::new();
    for base_var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Ok(base) = std::env::var(base_var) {
            candidates.push(PathBuf::from(&base).join("Git").join("bin").join("bash.exe"));
        }
    }
    candidates.push(PathBuf::from(r"C:\Git\bin\bash.exe"));

    if let Ok(output) = Command::new("where").arg("git").output() {
        if output.status.success() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    // .../Git/cmd/git.exe or .../Git/mingw64/bin/git.exe: root is two levels up.
                    if let Some(root) = Path::new(line).parent().and_then(Path::parent) {
                        candidates.push(root.join("bin").join("bash.exe"));
                        candidates.push(root.join("usr").join("bin").join("bash.exe"));
                    }
                }
            }
        }
    }

    candidates.into_iter().find(is_bash)
}

/// Spawn an ACP server process (Cursor/Claude `… acp`, or a standalone adapter).
pub fn spawn_acp_process(
    host: AcpHost,
    agent_bin: &Path,
    env: &[(String, String)],
) -> Result<crate::process_tree::AgentProcess> {
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

    #[cfg(windows)]
    if host == AcpHost::Claude && std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").is_none() {
        if let Some(bash) = resolve_git_bash_path() {
            command.env("CLAUDE_CODE_GIT_BASH_PATH", bash);
        }
    }

    // Claude Code marks its own shell with this, and Claude refuses to start
    // inside one ("cannot be launched inside another Claude Code session").
    // tod launched from a Claude Code session would otherwise hand it to every
    // agent it starts, which then fail with only "Query closed before response
    // received". The agent is tod's child, not a nested session.
    command.env_remove("CLAUDECODE");

    // Git Bash's login profile rebuilds PATH from ORIGINAL_PATH when that is
    // already set, and every Git Bash terminal exports it. tod launched from
    // one would hand it down, and the agent's shell would drop whatever the
    // caller added to PATH (tod-cli's directory among them). Without it, the
    // profile starts from the PATH the agent was actually given.
    command.env_remove("ORIGINAL_PATH");

    for (key, value) in env {
        command.env(key, value);
    }

    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    crate::process_tree::spawn(&mut command).with_context(|| {
        format!(
            "failed to spawn {} ACP ({})",
            host.label(),
            agent_bin.display()
        )
    })
}

/// The ACP agent inside a dev container: found on the container's `PATH`
/// rather than on this machine, since the host's may be a Windows shim.
pub fn container_agent_bin(
    host: AcpHost,
    container: &crate::devcontainer::PreparedContainer,
) -> Result<String> {
    let (candidates, install): (&[&str], &str) = match host {
        AcpHost::Claude => (
            &[crate::claude_adapter::BIN],
            "npm install -g @agentclientprotocol/claude-agent-acp (and sign in to `claude` there)",
        ),
        AcpHost::Cursor => (
            &["cursor-agent", "agent"],
            "curl https://cursor.com/install -fsS | bash (and sign in there)",
        ),
    };
    container.find_program(candidates)?.with_context(|| {
        format!(
            "No {} ACP agent in dev container `{}` (looked for {}). Install it in the container: {install}",
            host.label(),
            container.name,
            candidates.join(", "),
        )
    })
}

/// The ACP agent for `host` in a cloud sandbox.
pub fn sandbox_agent_bin(host: AcpHost, launch: &crate::sandbox::SandboxLaunch) -> Result<String> {
    let (candidates, install): (&[&str], &str) = match host {
        AcpHost::Claude => (
            &[crate::claude_adapter::BIN],
            "create the sandbox with `tod-sandbox create <name> --agents`, then sign in to `claude` there",
        ),
        AcpHost::Cursor => (
            &["cursor-agent", "agent"],
            "curl https://cursor.com/install -fsS | bash (and sign in there)",
        ),
    };
    launch.find_program(candidates)?.with_context(|| {
        format!(
            "No {} ACP agent in sandbox `{}` (looked for {}). Install it there: {install}",
            host.label(),
            launch.sandbox,
            candidates.join(", "),
        )
    })
}

/// [`spawn_acp_process`] in a cloud sandbox, through `tod-sandbox agent`.
/// ACP runs over the launcher's stdio exactly as it does over a local child's.
pub fn spawn_acp_in_sandbox(
    host: AcpHost,
    launch: &crate::sandbox::SandboxLaunch,
    agent_bin: &str,
    env: &[(String, String)],
) -> Result<crate::process_tree::AgentProcess> {
    use std::process::Stdio;

    let args: Vec<String> = if host.uses_acp_subcommand(Path::new(agent_bin)) {
        vec!["acp".to_string()]
    } else {
        Vec::new()
    };
    let mut command = launch.agent_command(agent_bin, &args, env);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::process_tree::spawn(&mut command).with_context(|| {
        format!(
            "failed to start {} ACP in sandbox `{}` ({})",
            host.label(),
            launch.sandbox,
            launch.launcher.display()
        )
    })
}

/// [`spawn_acp_process`] inside a dev container, through `docker exec -i`.
/// ACP runs over the exec's stdio exactly as it does over a local child's.
pub fn spawn_acp_in_container(
    host: AcpHost,
    container: &crate::devcontainer::PreparedContainer,
    agent_bin: &str,
    env: &[(String, String)],
) -> Result<crate::process_tree::AgentProcess> {
    use std::process::Stdio;

    let args: Vec<String> = if host.uses_acp_subcommand(Path::new(agent_bin)) {
        vec!["acp".to_string()]
    } else {
        Vec::new()
    };
    let mut container = container.clone();
    // A host `PATH` means nothing in the container; its own was set up by
    // `devcontainer::prepare`.
    for (key, value) in env.iter().filter(|(key, _)| !key.eq_ignore_ascii_case("PATH")) {
        container.env.retain(|(existing, _)| existing != key);
        container.env.push((key.clone(), value.clone()));
    }
    let mut command = container.command(agent_bin, &args)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::process_tree::spawn(&mut command).with_context(|| {
        format!(
            "failed to start {} ACP in dev container `{}`",
            host.label(),
            container.name
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

    #[cfg(windows)]
    #[test]
    fn spawn_acp_process_sets_git_bash_path_for_claude() {
        use std::io::Read;

        let Some(expected_bash) = resolve_git_bash_path() else {
            // Nothing to verify on a machine without Git for Windows installed.
            return;
        };

        let dir = std::env::temp_dir().join(format!("tod-acp-host-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude-agent-acp.cmd");
        std::fs::write(&script, "@echo %CLAUDE_CODE_GIT_BASH_PATH%\r\n").unwrap();

        let mut child = spawn_acp_process(AcpHost::Claude, &script, &[]).expect("spawn should succeed");
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        child.wait().unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(stdout.trim(), expected_bash.to_string_lossy());
    }

    #[cfg(windows)]
    #[test]
    fn finds_git_bash_when_installed() {
        // Sanity check for CLAUDE_CODE_GIT_BASH_PATH resolution: only meaningful
        // on a machine with Git for Windows installed (true for our CI/dev images).
        if let Some(path) = resolve_git_bash_path() {
            assert!(path.is_file(), "resolved path does not exist: {path:?}");
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some("bash.exe")
            );
        }
    }

    #[test]
    fn claudes_adapter_speaks_acp_without_a_subcommand() {
        assert!(!AcpHost::Claude.uses_acp_subcommand(Path::new("/usr/local/bin/claude-agent-acp")));
        assert!(AcpHost::Cursor.uses_acp_subcommand(Path::new("/usr/local/bin/agent")));
    }
}
