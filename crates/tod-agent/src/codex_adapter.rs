//! TOD's managed install of the Codex ACP adapter and sign-in CLI.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::time::Duration;

pub(crate) const INSTALL_SCRIPT: &str = include_str!("../../../assets/agent/codex-adapter.sh");
/// [`INSTALL_SCRIPT`] for the Claude adapter (`claude-agent-acp`), installed
/// into `tod/adapters/claude-agent-acp` in the environment's data directory.
/// Run it where the adapter will run (container, sandbox); the host keeps
/// `claude_adapter`'s own install.
pub(crate) fn claude_install_script() -> String {
    format!("TOD_ADAPTER=claude\n{INSTALL_SCRIPT}")
}

const INSTALL_TIMEOUT: Duration = Duration::from_secs(240);
const BIN_ENV: &str = "TOD_CODEX_ACP_BIN";
static LOCAL_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

pub fn set_local_dir(dir: Option<PathBuf>) {
    *LOCAL_DIR.write().unwrap_or_else(|err| err.into_inner()) = dir;
}

fn local_dir() -> Option<PathBuf> {
    LOCAL_DIR
        .read()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}

/// Ensure the adapter is available on this machine, installing or refreshing
/// tod's managed copy when needed. Also used by the app's startup check.
pub fn resolve_bin() -> Result<PathBuf> {
    if let Some(bin) = std::env::var_os(BIN_ENV).filter(|value| !value.is_empty()) {
        return Ok(bin.into());
    }
    install_host(local_dir().as_deref()).or_else(|err| {
        if let Ok(bin) = crate::acp_host::resolve_new_bin("codex-acp", "Install @agentclientprotocol/codex-acp") {
            tracing::warn!(error = %err, "using the existing Codex adapter after TOD's managed install failed");
            return Ok(bin);
        }
        Err(err)
    })
}

#[cfg(unix)]
fn install_host(prefix: Option<&Path>) -> Result<PathBuf> {
    let mut command = Command::new("sh");
    command.args(["-lc", INSTALL_SCRIPT, "tod-codex-adapter"]);
    if let Some(prefix) = prefix {
        command.arg(prefix);
    }
    installed_path(command, "this machine").map(PathBuf::from)
}

#[cfg(windows)]
fn install_host(prefix: Option<&Path>) -> Result<PathBuf> {
    use fs2::FileExt;
    let prefix = prefix.context("could not determine where TOD installs its Codex ACP adapter")?;
    let bin = prefix.join("node_modules/.bin/codex-acp.cmd");
    let cli = prefix.join("node_modules/.bin/codex.cmd");
    let stamp = prefix.join(".checked");
    let ready = || bin.is_file() && cli.is_file();
    let fresh = || {
        ready()
            && stamp
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|time| time.elapsed().ok())
                .is_some_and(|age| age < Duration::from_secs(86_400))
    };
    if !fresh() {
        std::fs::create_dir_all(prefix)?;
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(prefix.join(".install.lock"))?;
        lock.lock_exclusive()?;
        if !fresh() {
            let mut command = Command::new("cmd");
            command.current_dir(prefix).args([
                "/C",
                "npm",
                "install",
                "--no-audit",
                "--no-fund",
                "--fetch-retries=1",
                "--fetch-timeout=30000",
                "@agentclientprotocol/codex-acp@latest",
                "@openai/codex@latest",
            ]);
            let result = install_output(command, "this machine").and_then(|_| {
                anyhow::ensure!(
                    ready(),
                    "npm did not install the Codex adapter and CLI in {}",
                    prefix.display()
                );
                std::fs::write(&stamp, []).context("record the Codex ACP update check")
            });
            if let Err(err) = result {
                if !bin.is_file() {
                    return Err(err);
                }
                tracing::warn!(error = %err, "using the cached Codex adapter");
            }
        }
    }
    Ok(bin)
}

pub(crate) fn installed_path(command: Command, environment: &str) -> Result<String> {
    let stdout = install_output(command, environment)?;
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| line.starts_with('/'))
        .map(str::to_owned)
        .with_context(|| format!("ACP adapter setup in {environment} did not return an adapter path"))
}

fn install_output(mut command: Command, environment: &str) -> Result<String> {
    command.stdin(Stdio::null());
    let operation = format!("install or update the ACP adapter in {environment}");
    let output = crate::sandbox::output_within(command, INSTALL_TIMEOUT, &operation)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!(
            "Could not {operation} ({}): {}",
            output.status,
            stderr.trim()
        );
    }
    // The installer can still return a cached adapter after a registry error.
    if !stderr.trim().is_empty() {
        tracing::info!(%environment, details = %stderr.trim(), "ACP adapter installer");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn login_command(remote: bool) -> String {
    if remote {
        return r#""${XDG_DATA_HOME:-$HOME/.local/share}/tod/adapters/codex-acp/node_modules/.bin/codex" login --device-auth"#.into();
    }
    local_dir()
        .map(|dir| {
            let cli = dir.join(if cfg!(windows) {
                "node_modules/.bin/codex.cmd"
            } else {
                "node_modules/.bin/codex"
            });
            if cli.is_file() {
                return format!("\"{}\" login", cli.display());
            }
            "codex login".into()
        })
        .unwrap_or_else(|| "codex login".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::{self, File, FileTimes};
    use std::os::unix::fs::PermissionsExt;
    use std::time::SystemTime;

    #[test]
    fn managed_adapter_installs_once_refreshes_when_stale_and_survives_a_registry_error() {
        let root = tod_test_util::scratch_dir("codex-adapter");
        let prefix = root.join("managed adapter");
        let npm = root.join("npm");
        fs::write(
            &npm,
            r#"#!/bin/sh
set -eu
printf 'install\n' >> installs
[ "${REGISTRY_OFFLINE:-}" != 1 ] || exit 1
mkdir -p node_modules/.bin
for bin in codex-acp codex; do
    printf '#!/bin/sh\nexit 0\n' > "node_modules/.bin/$bin"
    chmod 755 "node_modules/.bin/$bin"
done
"#,
        )
        .unwrap();
        fs::set_permissions(&npm, fs::Permissions::from_mode(0o755)).unwrap();
        let ensure = |offline: bool| {
            let mut command = Command::new("sh");
            command
                .args(["-c", INSTALL_SCRIPT, "tod-codex-adapter"])
                .arg(&prefix)
                .env(
                    "PATH",
                    std::env::join_paths([root.clone(), "/usr/bin".into(), "/bin".into()]).unwrap(),
                )
                .env("REGISTRY_OFFLINE", if offline { "1" } else { "0" });
            installed_path(command, "the test environment").unwrap()
        };
        let count = || {
            fs::read_to_string(prefix.join("installs"))
                .unwrap()
                .lines()
                .count()
        };
        let stamp = prefix.join(".checked");
        let age_install = || {
            File::options()
                .write(true)
                .open(&stamp)
                .unwrap()
                .set_times(
                    FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(172_800)),
                )
                .unwrap();
        };
        let adapter = prefix.join("node_modules/.bin/codex-acp");
        assert_eq!(Path::new(&ensure(false)), adapter);
        assert_eq!(Path::new(&ensure(false)), adapter);
        assert_eq!(count(), 1);
        age_install();
        assert_eq!(Path::new(&ensure(false)), adapter);
        assert_eq!(count(), 2);
        age_install();
        assert_eq!(Path::new(&ensure(true)), adapter);
        assert_eq!(count(), 3);
    }

    fn claude_run(root: &Path, prefix: &Path, path: &[PathBuf]) -> Result<String> {
        let mut command = Command::new("sh");
        command
            .args(["-c", &claude_install_script(), "tod-claude-adapter"])
            .arg(prefix)
            .env("PATH", std::env::join_paths(path).unwrap())
            .env_remove("TOD_ADAPTER")
            .current_dir(root);
        installed_path(command, "the test environment")
    }

    fn fake_npm(root: &Path, body: &str) {
        let npm = root.join("npm");
        fs::write(&npm, format!("#!/bin/sh\nset -eu\nprintf 'install %s\\n' \"$*\" >> installs\n{body}")).unwrap();
        fs::set_permissions(&npm, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn claude_adapter_is_installed_into_tods_own_prefix_with_no_codex_packages() {
        let root = tod_test_util::scratch_dir("claude-adapter");
        let prefix = root.join("claude-agent-acp");
        fake_npm(
            &root,
            "mkdir -p node_modules/.bin\nprintf '#!/bin/sh\\nexit 0\\n' > node_modules/.bin/claude-agent-acp\nchmod 755 node_modules/.bin/claude-agent-acp\n",
        );
        let path = [root.clone(), "/usr/bin".into(), "/bin".into()];
        let bin = claude_run(&root, &prefix, &path).unwrap();
        assert_eq!(Path::new(&bin), prefix.join("node_modules/.bin/claude-agent-acp"));
        // Fresh: the second call does not run npm again.
        claude_run(&root, &prefix, &path).unwrap();
        let installs = fs::read_to_string(prefix.join("installs")).unwrap();
        assert_eq!(installs.lines().count(), 1);
        assert!(installs.contains("@agentclientprotocol/claude-agent-acp@latest"), "{installs}");
        assert!(!installs.contains("codex"), "{installs}");
    }

    #[test]
    fn claude_adapter_on_path_is_used_only_when_the_managed_install_fails() {
        let root = tod_test_util::scratch_dir("claude-adapter-fallback");
        let prefix = root.join("managed");
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let existing = elsewhere.join("claude-agent-acp");
        fs::write(&existing, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
        fake_npm(&root, "exit 1\n");
        let path = [root.clone(), elsewhere, "/usr/bin".into(), "/bin".into()];
        let bin = claude_run(&root, &prefix, &path).unwrap();
        assert_eq!(Path::new(&bin), existing);
    }

    #[test]
    fn claude_adapter_without_npm_says_so() {
        let root = tod_test_util::scratch_dir("claude-adapter-no-npm");
        let prefix = root.join("managed");
        let err = claude_run(&root, &prefix, &["/usr/bin".into(), "/bin".into()]).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("npm is not installed"), "{text}");
        assert!(text.contains("Claude"), "{text}");
    }
}
