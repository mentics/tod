//! Dev containers: running agents and shells inside a running Docker container.
//!
//! Everything here talks to the `docker` CLI on the host. Nothing starts,
//! builds, or stops a container: the user runs their dev container, and tod
//! only lists the running ones, inspects the one a node names, and `docker
//! exec`s into it.
//!
//! A node's repository usually lives inside the container, and its directory
//! is a container path. When the repository is on this machine and mounted
//! into the container instead, the directory an agent works in is found by
//! mapping the host path through the container's mounts
//! ([`ContainerInfo::map_host_path`]), unless the node names one explicitly.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Where an agent process runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AgentEnvironment {
    /// On this machine, in the turn's `cwd`.
    #[default]
    Host,
    /// Inside a running dev container, via `docker exec`.
    DevContainer(DevContainerLaunch),
}

impl AgentEnvironment {
    pub fn dev_container(&self) -> Option<&DevContainerLaunch> {
        match self {
            Self::Host => None,
            Self::DevContainer(launch) => Some(launch),
        }
    }
}

/// How to start a process inside a dev container. Resolved against the
/// running container by [`prepare`], off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevContainerLaunch {
    /// Container name or id.
    pub container: String,
    /// The host directory the process would run in on this machine; mapped
    /// through the container's mounts when `directory` is unset (ignored
    /// when it is set).
    pub host_dir: PathBuf,
    /// The directory inside the container, when it is known: the node names
    /// one, or the repository lives in the container.
    pub directory: Option<String>,
    /// Environment for the process, on top of the container's own.
    pub env: Vec<(String, String)>,
    /// Directories put ahead of the container's `PATH`.
    pub path_prepend: Vec<String>,
    /// Files written into the container (as root) before the process starts.
    pub files: Vec<ContainerFile>,
}

/// A file [`prepare`] writes into the container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerFile {
    /// Absolute path inside the container.
    pub path: String,
    pub contents: String,
    pub executable: bool,
}

/// A [`DevContainerLaunch`] checked against the running container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedContainer {
    pub id: String,
    pub name: String,
    /// The user processes run as (the dev container's `remoteUser`), or the
    /// container's default user when `None`.
    pub user: Option<String>,
    /// The working directory inside the container.
    pub cwd: String,
    /// `PATH` for the process: the prepended directories, then the
    /// container's own.
    pub path: String,
    pub env: Vec<(String, String)>,
}

impl PreparedContainer {
    /// `docker exec` arguments (after `exec`) that run `program args…` in
    /// the container. Environment values are passed by name only (`-e KEY`)
    /// and must be set on the `docker` process itself — see [`Self::command`]
    /// — so they never appear on a command line.
    pub fn exec_args(&self, interactive_tty: bool) -> Vec<String> {
        let mut args = vec!["exec".to_string()];
        args.push(if interactive_tty { "-it" } else { "-i" }.to_string());
        if let Some(user) = &self.user {
            args.extend(["-u".to_string(), user.clone()]);
        }
        args.extend(["-w".to_string(), self.cwd.clone()]);
        args.extend(["-e".to_string(), "PATH".to_string()]);
        for (key, _) in &self.env {
            args.extend(["-e".to_string(), key.clone()]);
        }
        args.push(self.id.clone());
        args
    }

    /// A `docker exec -i` command running `program args…` in the container,
    /// with its environment set. The caller sets stdio and spawns it.
    pub fn command(&self, program: &str, args: &[String]) -> Result<Command> {
        let mut command = docker_command()?;
        command.args(self.exec_args(false));
        command.arg(program).args(args);
        command.env("PATH", &self.path);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        Ok(command)
    }

    /// The first of `candidates` found on the container's `PATH`, run as the
    /// container user.
    pub fn find_program(&self, candidates: &[&str]) -> Result<Option<String>> {
        let script = candidates
            .iter()
            .map(|name| format!("command -v {name} 2>/dev/null && exit 0;"))
            .collect::<String>()
            + " exit 1";
        let mut command = self.command("sh", &["-c".to_string(), script])?;
        let out = output(&mut command)?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string))
    }
}

/// A running container, as `docker ps` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
    /// The host folder a dev container was opened from (the
    /// `devcontainer.local_folder` label VS Code and the devcontainer CLI set).
    pub local_folder: Option<String>,
}

impl ContainerSummary {
    pub fn is_dev_container(&self) -> bool {
        self.local_folder.is_some()
    }
}

/// A container's details, from `docker inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub running: bool,
    /// The dev container's `remoteUser` (else `containerUser`) from its
    /// `devcontainer.metadata` label.
    pub remote_user: Option<String>,
    pub mounts: Vec<Mount>,
    /// `PATH` from the container's configured environment.
    pub path: Option<String>,
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Host side, as Docker reports it.
    pub source: String,
    /// Absolute path inside the container.
    pub destination: String,
}

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A container name or id is passed to `docker` and written into shell
/// commands, so only Docker's own name characters are accepted.
pub fn validate_container_ref(container: &str) -> Result<()> {
    let mut chars = container.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        bail!("`{container}` is not a container name or id");
    }
    Ok(())
}

/// The `docker` CLI: `TOD_DOCKER_BIN`, else `docker` on `PATH`, else where
/// Docker Desktop installs it (a GUI-launched app does not always inherit a
/// `PATH` that has it).
pub fn docker_bin() -> PathBuf {
    if let Some(bin) = std::env::var_os("TOD_DOCKER_BIN").filter(|b| !b.is_empty()) {
        return PathBuf::from(bin);
    }
    let name = if cfg!(windows) { "docker.exe" } else { "docker" };
    if let Some(path) = std::env::var_os("PATH") {
        if let Some(found) = std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
        {
            return found;
        }
    }
    let mut candidates = Vec::new();
    if cfg!(windows) {
        for base in ["ProgramFiles", "ProgramW6432"] {
            if let Some(dir) = std::env::var_os(base) {
                candidates.push(
                    PathBuf::from(dir)
                        .join("Docker")
                        .join("Docker")
                        .join("resources")
                        .join("bin")
                        .join(name),
                );
            }
        }
    } else {
        candidates.push(PathBuf::from("/usr/local/bin/docker"));
        candidates.push(PathBuf::from("/opt/homebrew/bin/docker"));
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".docker").join("bin").join(name));
        }
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn docker_command() -> Result<Command> {
    let mut command = Command::new(docker_bin());
    no_window(&mut command);
    Ok(command)
}

#[cfg(windows)]
fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_command: &mut Command) {}

fn output(command: &mut Command) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .output()
        .with_context(|| "run docker (is Docker installed and running?)")
}

fn docker(args: &[&str]) -> Result<String> {
    let out = output(docker_command()?.args(args))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("docker {}: {err}", args.first().copied().unwrap_or_default());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Running containers, dev containers first.
pub fn list_running() -> Result<Vec<ContainerSummary>> {
    let text = docker(&["ps", "--no-trunc", "--format", "{{json .}}"])?;
    let mut containers: Vec<ContainerSummary> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<PsLine>(line).ok())
        .map(ContainerSummary::from)
        .collect();
    containers.sort_by(|a, b| {
        b.is_dev_container()
            .cmp(&a.is_dev_container())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(containers)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PsLine {
    #[serde(rename = "ID")]
    id: String,
    names: String,
    image: String,
    status: String,
    #[serde(default)]
    labels: String,
}

impl From<PsLine> for ContainerSummary {
    fn from(line: PsLine) -> Self {
        // `docker ps` joins labels with commas; a label value may hold a
        // comma too, but the local folder is a path and rarely does.
        let local_folder = line
            .labels
            .split(',')
            .find_map(|label| label.strip_prefix("devcontainer.local_folder="))
            .map(str::to_string)
            .filter(|folder| !folder.is_empty());
        Self {
            id: line.id.chars().take(12).collect(),
            name: line
                .names
                .split(',')
                .next()
                .unwrap_or_default()
                .to_string(),
            image: line.image,
            status: line.status,
            local_folder,
        }
    }
}

/// `docker inspect` one container by name or id.
pub fn inspect(container: &str) -> Result<ContainerInfo> {
    validate_container_ref(container)?;
    let out = output(docker_command()?.args(["inspect", "--type", "container", container]))?;
    if !out.status.success() {
        bail!(
            "No container named `{container}` (docker: {})",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let value: Value = serde_json::from_slice(&out.stdout).context("parse docker inspect")?;
    let item = value
        .as_array()
        .and_then(|items| items.first())
        .context("docker inspect returned nothing")?;
    Ok(parse_inspect(item))
}

fn parse_inspect(item: &Value) -> ContainerInfo {
    let str_at = |pointer: &str| {
        item.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let mounts = item
        .get("Mounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|mount| {
            Some(Mount {
                source: mount.get("Source")?.as_str()?.to_string(),
                destination: mount.get("Destination")?.as_str()?.to_string(),
            })
        })
        .collect();
    let path = item
        .pointer("/Config/Env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|entry| entry.strip_prefix("PATH="))
        .map(str::to_string);
    let remote_user = str_at("/Config/Labels/devcontainer.metadata")
        .and_then(|metadata| remote_user_from_metadata(&metadata));
    ContainerInfo {
        id: str_at("/Id")
            .map(|id| id.chars().take(12).collect())
            .unwrap_or_default(),
        name: str_at("/Name")
            .map(|name| name.trim_start_matches('/').to_string())
            .unwrap_or_default(),
        running: item
            .pointer("/State/Running")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        remote_user,
        mounts,
        path,
        working_dir: str_at("/Config/WorkingDir"),
    }
}

/// The user a dev container's tools run as: the last `remoteUser` in its
/// merged metadata (later entries override earlier ones), else the last
/// `containerUser`.
fn remote_user_from_metadata(metadata: &str) -> Option<String> {
    let value: Value = serde_json::from_str(metadata).ok()?;
    let entries: Vec<&Value> = match &value {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let last = |key: &str| {
        entries
            .iter()
            .rev()
            .find_map(|entry| entry.get(key)?.as_str())
            .filter(|user| !user.is_empty())
            .map(str::to_string)
    };
    last("remoteUser").or_else(|| last("containerUser"))
}

impl ContainerInfo {
    /// Where `host` is inside the container: under the mount whose source
    /// holds it most specifically. `None` when no mount holds it.
    pub fn map_host_path(&self, host: &Path) -> Option<String> {
        let host = comparable(&host.to_string_lossy());
        self.mounts
            .iter()
            .filter_map(|mount| {
                let source = comparable(&mount.source);
                let rest = strip_dir_prefix(&host, &source)?;
                Some((source.len(), join_posix(&mount.destination, rest)))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, path)| path)
    }
}

/// A host path in one form for comparison: forward slashes, no trailing
/// slash, and the several ways Docker Desktop writes a Windows or macOS
/// host path (`C:\x`, `/run/desktop/mnt/host/c/x`, `/mnt/c/x`,
/// `/host_mnt/c/x`, `/host_mnt/Users/x`) brought to one (`c:/x`, `/Users/x`).
/// Windows paths compare without case.
fn comparable(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    if let Some(rest) = p.strip_prefix("//?/") {
        p = rest.to_string();
    }
    for prefix in ["/run/desktop/mnt/host/", "/mnt/host/", "/host_mnt/", "/mnt/"] {
        if let Some(rest) = p.strip_prefix(prefix) {
            let mut parts = rest.splitn(2, '/');
            let first = parts.next().unwrap_or_default();
            if first.len() == 1 && first.chars().all(|c| c.is_ascii_alphabetic()) {
                p = format!("{first}:/{}", parts.next().unwrap_or_default());
            } else if prefix == "/host_mnt/" {
                p = format!("/{rest}");
            }
            break;
        }
    }
    let bytes = p.as_bytes();
    let windows = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    if windows {
        p = p.to_ascii_lowercase();
    }
    while p.len() > 1 && p.ends_with('/') && !p.ends_with(":/") {
        p.pop();
    }
    p
}

/// `path` relative to `dir` (`""` when equal), when `path` is `dir` or under it.
fn strip_dir_prefix<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(dir)?;
    if rest.is_empty() {
        return Some("");
    }
    if dir.ends_with('/') {
        return Some(rest);
    }
    rest.strip_prefix('/')
}

fn join_posix(dir: &str, rest: &str) -> String {
    if rest.is_empty() {
        return dir.to_string();
    }
    format!("{}/{rest}", dir.trim_end_matches('/'))
}

/// The running `container` and the directory in it a process for
/// `host_dir` runs in: `directory` when given, else `host_dir` mapped
/// through the container's mounts. Talks to Docker.
pub fn resolve_directory(
    container: &str,
    host_dir: &Path,
    directory: Option<&str>,
) -> Result<(ContainerInfo, String)> {
    let info = inspect(container)?;
    if !info.running {
        bail!("Dev container `{container}` is not running — start it, then try again");
    }
    let cwd = match directory.map(str::trim) {
        Some(dir) if !dir.is_empty() => dir.to_string(),
        _ => info.map_host_path(host_dir).with_context(|| {
            format!(
                "{} is not mounted in dev container `{container}`. Mount it, or set the                  directory in the container (Files).",
                host_dir.display(),
            )
        })?,
    };
    if !cwd.starts_with('/') {
        bail!("The directory in the container must be absolute: {cwd}");
    }
    Ok((info, cwd))
}

/// Check `launch` against the running container, work out its directory,
/// user, and `PATH`, and write its files. Talks to Docker: never call it on
/// the UI thread.
pub fn prepare(launch: &DevContainerLaunch) -> Result<PreparedContainer> {
    let (info, cwd) = resolve_directory(
        &launch.container,
        &launch.host_dir,
        launch.directory.as_deref(),
    )?;
    let mut path: Vec<String> = launch.path_prepend.clone();
    path.push(info.path.clone().unwrap_or_else(|| DEFAULT_PATH.to_string()));
    let prepared = PreparedContainer {
        id: info.id.clone(),
        name: info.name.clone(),
        user: info.remote_user.clone(),
        cwd,
        path: path.join(":"),
        env: launch.env.clone(),
    };
    for file in &launch.files {
        write_file(&prepared.id, file)?;
    }
    Ok(prepared)
}

/// Write `file` into `container` as root, replacing it atomically.
pub fn write_file(container: &str, file: &ContainerFile) -> Result<()> {
    validate_container_ref(container)?;
    let path = &file.path;
    if !path.starts_with('/') || path.contains('\'') {
        bail!("not a usable container path: {path}");
    }
    let dir = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("/");
    let mode = if file.executable { "755" } else { "644" };
    let script = format!(
        "mkdir -p '{dir}' && chmod 755 '{dir}' && cat > '{path}.tmp' && chmod {mode} '{path}.tmp' && mv -f '{path}.tmp' '{path}'"
    );
    let mut child = docker_command()?
        .args(["exec", "-i", "-u", "0", container, "sh", "-c", &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("run docker (is Docker installed and running?)")?;
    child
        .stdin
        .take()
        .context("docker stdin")?
        .write_all(file.contents.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "write {path} in dev container `{container}`: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Runs commands in a running container as its dev user (`remoteUser`), the
/// way a process started in it by tod would. [`ContainerExec::connect`]
/// inspects the container once and is cached for a short while, so a burst
/// of `git` calls costs one `docker inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerExec {
    pub id: String,
    pub name: String,
    pub user: Option<String>,
}

const EXEC_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

type ExecCache = std::collections::HashMap<String, (ContainerExec, std::time::Instant)>;

fn exec_cache() -> &'static std::sync::Mutex<ExecCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<ExecCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

impl ContainerExec {
    /// The running `container` (name or id). Talks to Docker unless cached.
    pub fn connect(container: &str) -> Result<Self> {
        validate_container_ref(container)?;
        if let Some((exec, at)) = exec_cache().lock().expect("exec cache").get(container) {
            if at.elapsed() < EXEC_CACHE_TTL {
                return Ok(exec.clone());
            }
        }
        let info = inspect(container)?;
        if !info.running {
            bail!("Dev container `{container}` is not running — start it, then try again");
        }
        let exec = Self {
            id: info.id,
            name: info.name,
            user: info.remote_user,
        };
        exec_cache()
            .lock()
            .expect("exec cache")
            .insert(container.to_string(), (exec.clone(), std::time::Instant::now()));
        Ok(exec)
    }

    /// `docker exec` running `program args…` in `dir` inside the container.
    /// Stdin is closed; the caller runs it.
    pub fn command(&self, dir: &str, program: &str, args: &[&str]) -> Result<Command> {
        let mut command = docker_command()?;
        command.args(["exec", "-w", dir]);
        if let Some(user) = &self.user {
            command.args(["-u", user]);
        }
        command.arg(&self.id).arg(program).args(args);
        command.stdin(Stdio::null());
        Ok(command)
    }

    /// Run `program args…` in `dir` and collect its output.
    pub fn output(&self, dir: &str, program: &str, args: &[&str]) -> Result<Output> {
        let mut command = self.command(dir, program, args)?;
        output(&mut command)
    }

    /// Whether `path` is a directory in the container.
    pub fn is_dir(&self, path: &str) -> Result<bool> {
        Ok(self.output("/", "test", &["-d", path])?.status.success())
    }
}

/// Single-quote `value` for a POSIX shell.
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info(mounts: &[(&str, &str)]) -> ContainerInfo {
        ContainerInfo {
            id: "abc".into(),
            name: "dev".into(),
            running: true,
            remote_user: None,
            mounts: mounts
                .iter()
                .map(|(source, destination)| Mount {
                    source: source.to_string(),
                    destination: destination.to_string(),
                })
                .collect(),
            path: None,
            working_dir: None,
        }
    }

    #[test]
    fn a_host_path_maps_through_the_most_specific_mount() {
        let info = info(&[
            (r"C:\data\git\proj", "/workspaces/proj"),
            (r"C:\data", "/data"),
        ]);
        assert_eq!(
            info.map_host_path(Path::new(r"C:\data\git\proj")).as_deref(),
            Some("/workspaces/proj")
        );
        assert_eq!(
            info.map_host_path(Path::new(r"c:\Data\git\proj\src\")).as_deref(),
            Some("/workspaces/proj/src")
        );
        assert_eq!(
            info.map_host_path(Path::new(r"C:\data\other")).as_deref(),
            Some("/data/other")
        );
        assert_eq!(info.map_host_path(Path::new(r"D:\elsewhere")), None);
        // A sibling that only shares a name prefix is not under the mount.
        assert_eq!(
            info.map_host_path(Path::new(r"C:\data\git\project2")).as_deref(),
            Some("/data/git/project2")
        );
    }

    #[test]
    fn docker_desktop_mount_sources_match_windows_and_mac_paths() {
        let info = info(&[
            ("/run/desktop/mnt/host/c/src/app", "/workspaces/app"),
            ("/host_mnt/Users/me/code", "/code"),
            ("/home/me/proj", "/proj"),
        ]);
        assert_eq!(
            info.map_host_path(Path::new(r"C:\src\app\lib")).as_deref(),
            Some("/workspaces/app/lib")
        );
        assert_eq!(
            info.map_host_path(Path::new("/Users/me/code/x")).as_deref(),
            Some("/code/x")
        );
        assert_eq!(
            info.map_host_path(Path::new("/home/me/proj")).as_deref(),
            Some("/proj")
        );
    }

    #[test]
    fn inspect_output_gives_user_mounts_and_path() {
        let item = json!({
            "Id": "0123456789abcdef",
            "Name": "/my-dev",
            "State": { "Running": true },
            "Config": {
                "Env": ["HOME=/root", "PATH=/usr/bin:/bin"],
                "WorkingDir": "",
                "Labels": {
                    "devcontainer.metadata":
                        "[{\"remoteUser\":\"root\"},{\"containerUser\":\"x\"},{\"remoteUser\":\"vscode\"}]"
                }
            },
            "Mounts": [{ "Source": "C:\\p", "Destination": "/workspaces/p" }]
        });
        let info = parse_inspect(&item);
        assert_eq!(info.id, "0123456789ab");
        assert_eq!(info.name, "my-dev");
        assert!(info.running);
        assert_eq!(info.remote_user.as_deref(), Some("vscode"));
        assert_eq!(info.path.as_deref(), Some("/usr/bin:/bin"));
        assert_eq!(info.working_dir, None);
        assert_eq!(info.mounts.len(), 1);
    }

    #[test]
    fn ps_lines_mark_dev_containers() {
        let line: PsLine = serde_json::from_value(json!({
            "ID": "0123456789abcdef0123",
            "Names": "my-dev",
            "Image": "mcr.microsoft.com/devcontainers/base",
            "Status": "Up 2 hours",
            "Labels": "a=b,devcontainer.local_folder=C:\\src\\app,c=d"
        }))
        .unwrap();
        let summary = ContainerSummary::from(line);
        assert_eq!(summary.id, "0123456789ab");
        assert_eq!(summary.local_folder.as_deref(), Some(r"C:\src\app"));
        assert!(summary.is_dev_container());
    }

    #[test]
    fn container_refs_are_names_or_ids_only() {
        assert!(validate_container_ref("my-dev_1.x").is_ok());
        assert!(validate_container_ref("0123abcd").is_ok());
        for bad in ["", "-x", "a b", "a;rm", "a'b", "/a"] {
            assert!(validate_container_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn exec_args_pass_environment_by_name_only() {
        let prepared = PreparedContainer {
            id: "abc".into(),
            name: "dev".into(),
            user: Some("vscode".into()),
            cwd: "/workspaces/p".into(),
            path: "/x:/usr/bin".into(),
            env: vec![("TOD_SECRET".into(), "s3cret".into())],
        };
        let args = prepared.exec_args(false);
        assert_eq!(
            args,
            [
                "exec", "-i", "-u", "vscode", "-w", "/workspaces/p", "-e", "PATH", "-e",
                "TOD_SECRET", "abc"
            ]
        );
        assert!(!args.iter().any(|a| a.contains("s3cret")));
    }
}
