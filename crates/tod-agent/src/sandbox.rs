//! Processes in a cloud sandbox, started through the `tod-sandbox` launcher
//! on this machine. The launcher bridges the process's stdio over the
//! sandbox's relay, so to this crate it is a local child like any other, and
//! it carries the sandbox's `tod-cli` back to this machine while attached.
//!
//! Which sandbox and launcher, and the relay endpoint for `tod-cli`, are
//! decided by the caller (`tod_store::fleet::dev_container`).

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// How to start a process in a cloud sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxLaunch {
    /// The `tod-sandbox` executable on this machine.
    pub launcher: PathBuf,
    /// The data root it reads the sandbox settings and sign-in from.
    pub data_root: PathBuf,
    /// The sandbox's name.
    pub sandbox: String,
    /// The directory in the sandbox.
    pub directory: String,
    /// Environment for the process in the sandbox.
    pub env: Vec<(String, String)>,
    /// The `tod-cli` relay's environment on this machine (its port and
    /// token). The launcher carries the sandbox's `tod-cli` back to it.
    pub cli_relay: Vec<(String, String)>,
}

impl SandboxLaunch {
    fn launcher_command(&self) -> Command {
        let mut command = Command::new(&self.launcher);
        command.arg("--data-root").arg(&self.data_root);
        no_window(&mut command);
        command
    }

    /// The first of `candidates` found in the sandbox, on its `PATH` or a
    /// login shell's. Makes the sandbox ready first, if it is not.
    pub fn find_program(&self, candidates: &[&str]) -> Result<Option<String>> {
        let script = candidates
            .iter()
            .map(|name| format!("command -v {name} 2>/dev/null && exit 0;"))
            .collect::<String>()
            // Not 1, which is also the launcher's own failure.
            + " exit 3";
        let mut command = self.launcher_command();
        command
            .args(["exec", &self.sandbox, "--", "sh", "-lc", &script])
            .stdin(Stdio::null());
        let out = command
            .output()
            .with_context(|| format!("run {}", self.launcher.display()))?;
        match out.status.code() {
            Some(0) => {}
            Some(3) => return Ok(None),
            _ => bail!(
                "could not reach sandbox {}: {}",
                self.sandbox,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .rfind(|line| line.starts_with('/'))
            .map(str::to_string))
    }

    /// `program args…` in the sandbox, over stdio, as `tod-sandbox agent`
    /// runs it: it outlives a dropped connection and lets the sandbox sleep
    /// between requests. `env` is added to [`Self::env`] (a host `PATH` is
    /// left out: it means nothing there).
    pub fn agent_command(&self, program: &str, args: &[String], env: &[(String, String)]) -> Command {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "acp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mut command = self.launcher_command();
        command
            .args(["agent", &self.sandbox, "--name", &name])
            .arg("--cwd")
            .arg(&self.directory);
        if !self.cli_relay.is_empty() {
            command.arg("--cli-relay");
            command.envs(self.cli_relay.iter().map(|(k, v)| (k, v)));
        }
        // Values go on this process, names on the command line, so nothing
        // secret shows in a process list.
        let mut all: Vec<&(String, String)> = self.env.iter().collect();
        for pair in env {
            all.retain(|(key, _)| key != &pair.0);
            all.push(pair);
        }
        for (key, value) in all {
            if key.eq_ignore_ascii_case("PATH") {
                continue;
            }
            command.arg("--env").arg(key).env(key, value);
        }
        command.arg("--").arg(program).args(args);
        command
    }
}

#[cfg(windows)]
fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> SandboxLaunch {
        SandboxLaunch {
            launcher: PathBuf::from("tod-sandbox"),
            data_root: PathBuf::from("/data"),
            sandbox: "dev".into(),
            directory: "/root/app".into(),
            env: vec![("A".into(), "1".into())],
            cli_relay: vec![("TOD_CLI_RELAY_PORT".into(), "4000".into())],
        }
    }

    #[test]
    fn agent_command_names_env_and_carries_values_on_the_process() {
        let env = [
            ("A".to_string(), "2".to_string()),
            ("PATH".to_string(), "/host".to_string()),
            ("SECRET".to_string(), "s".to_string()),
        ];
        let command = launch().agent_command("/usr/bin/claude-agent-acp", &[], &env);
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(joined.starts_with("--data-root /data agent dev --name acp-"), "{joined}");
        assert!(joined.contains("--cwd /root/app --cli-relay --env A --env SECRET -- /usr/bin/claude-agent-acp"));
        assert!(!joined.contains("PATH") && !joined.contains(" s "));
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| *k == "A" && *v == Some("2".as_ref())));
        assert!(envs.iter().any(|(k, _)| *k == "TOD_CLI_RELAY_PORT"));
    }
}
