//! Reaching a dev container over SSH, so an editor can open code inside it.
//!
//! `ssh` talks to `sshd -i` started in the container by `docker exec` (a
//! `ProxyCommand`): no port and no network. tod keeps everything this needs
//! under `<data root>/ssh/`: a key of its own, a `config` with one
//! `Host tod-<container>` entry per container, and a `known_hosts` holding each
//! container's own host key, read from the container when it is readied.
//!
//! Editors run the user's `ssh`, which reads only `~/.ssh/config`. That file
//! needs one `Include` line naming tod's `config`, which tod adds only when the
//! user agrees ([`SshIncludeNeeded`], [`add_include`]).

use super::RemoteHost;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tod_agent::devcontainer::{self, ContainerExec};

const CONFIG_HEADER: &str = "# Written by tod: one host per dev container it opens code in.\n\
# Edits here are overwritten.\n";

/// `<data root>/ssh`, where tod keeps its key, `config`, and `known_hosts`.
pub fn ssh_dir(data_root: &Path) -> PathBuf {
    data_root.join("ssh")
}

/// The user's own `ssh` config: `TOD_SSH_CONFIG` when set (tests and dev
/// runs), else `~/.ssh/config`.
pub fn user_ssh_config() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TOD_SSH_CONFIG").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = dirs::home_dir().context("no home directory to find ~/.ssh/config in")?;
    Ok(home.join(".ssh").join("config"))
}

/// A path as `ssh` config wants it: forward slashes, no `\\?\` prefix.
fn config_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.replace('\\', "/")
}

/// How `ssh` builds name tod's `config`. On Windows the editor may run
/// Git's MSYS `ssh`, which reads `C:/…` in an `Include` as relative to
/// `~/.ssh` and wants `/c/…`, while Windows' own OpenSSH wants `C:/…`. Each
/// skips the form it finds no file at.
fn include_paths(data_root: &Path) -> Vec<String> {
    let path = config_path(&ssh_dir(data_root).join("config"));
    let mut paths = vec![path.clone()];
    let bytes = path.as_bytes();
    if cfg!(windows) && bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && &path[1..3] == ":/" {
        paths.push(format!("/{}{}", path[..1].to_ascii_lowercase(), &path[2..]));
    }
    paths
}

/// The line `~/.ssh/config` needs so `ssh` reads tod's hosts.
pub fn include_line(data_root: &Path) -> String {
    let paths: Vec<String> = include_paths(data_root)
        .iter()
        .map(|path| format!("\"{path}\""))
        .collect();
    format!("Include {}", paths.join(" "))
}

const INCLUDE_COMMENT: &str = "# Added by tod: its dev container hosts (tod-*).";

/// `~/.ssh/config` does not include tod's `config` yet: ask the user, then
/// [`add_include`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshIncludeNeeded {
    pub user_config: PathBuf,
    pub line: String,
    pub data_root: PathBuf,
}

impl std::fmt::Display for SshIncludeNeeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ssh does not read tod's dev container hosts: {} needs the line `{}`",
            self.user_config.display(),
            self.line
        )
    }
}

impl std::error::Error for SshIncludeNeeded {}

/// Whether `user_config` already includes tod's `config`, in every form
/// [`include_paths`] names.
pub fn has_include(user_config: &Path, data_root: &Path) -> Result<bool> {
    let text = match std::fs::read_to_string(user_config) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("read {}", user_config.display())),
    };
    let ours = include_paths(data_root);
    Ok(text.lines().any(|line| included(line, &ours) == ours.len()))
}

/// How many of `ours` the config line `line` includes.
fn included(line: &str, ours: &[String]) -> usize {
    let line = line.trim();
    let Some(keyword) = line.split(|c: char| c.is_whitespace() || c == '=').next() else {
        return 0;
    };
    if !keyword.eq_ignore_ascii_case("include") {
        return 0;
    }
    let args: Vec<String> = config_args(
        line[keyword.len()..].trim_start_matches(|c: char| c.is_whitespace() || c == '='),
    )
    .iter()
    .map(|arg| comparable(arg))
    .collect();
    ours.iter()
        .filter(|path| args.contains(&comparable(path)))
        .count()
}

/// A config line's arguments: whitespace-separated, or whole in `"…"`.
fn config_args(text: &str) -> Vec<&str> {
    text.split('"')
        .enumerate()
        .flat_map(|(i, part)| {
            if i % 2 == 1 {
                vec![part]
            } else {
                part.split_whitespace().collect()
            }
        })
        .collect()
}

fn comparable(path: &str) -> String {
    let path = path.replace('\\', "/");
    if cfg!(windows) {
        path.to_ascii_lowercase()
    } else {
        path
    }
}

/// Fail with [`SshIncludeNeeded`] unless `~/.ssh/config` includes tod's
/// `config`.
pub fn require_include(data_root: &Path) -> Result<()> {
    let user_config = user_ssh_config()?;
    if has_include(&user_config, data_root)? {
        return Ok(());
    }
    Err(SshIncludeNeeded {
        user_config,
        line: include_line(data_root),
        data_root: data_root.to_path_buf(),
    }
    .into())
}

/// Put the `Include` line at the top of `user_config`, creating it (and its
/// directory) if need be, and drop any older line of tod's that names only
/// some of the forms. At the top, before any `Host`, it applies to every
/// host, and `ssh` takes tod's settings for `tod-*` first. The file is
/// rewritten in place, so its permissions, and a symlink to it, stay as they
/// are.
pub fn add_include(user_config: &Path, data_root: &Path) -> Result<()> {
    if has_include(user_config, data_root)? {
        return Ok(());
    }
    let existing = match std::fs::read_to_string(user_config) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", user_config.display())),
    };
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let ours = include_paths(data_root);
    let rest: Vec<&str> = existing
        .lines()
        .filter(|line| line.trim() != INCLUDE_COMMENT && included(line, &ours) == 0)
        .skip_while(|line| line.trim().is_empty())
        .collect();
    let mut text = format!(
        "{INCLUDE_COMMENT}{newline}{}{newline}",
        include_line(data_root)
    );
    if !rest.is_empty() {
        text.push_str(newline);
        for line in rest {
            text.push_str(line);
            text.push_str(newline);
        }
    }
    if let Some(dir) = user_config.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(user_config, text).with_context(|| format!("write {}", user_config.display()))
}

/// Ready `container` for `ssh` and record it in tod's `config`. Returns the
/// host to connect to. Talks to Docker; never call it on the UI thread.
pub fn connect_container(data_root: &Path, container: &str) -> Result<RemoteHost> {
    let exec = ContainerExec::connect(container)?;
    let user = match exec.user.clone() {
        Some(user) => user,
        None => container_user(&exec)?,
    };
    let dir = ssh_dir(data_root);
    ensure_private_dir(&dir)?;
    let key = ensure_key(&dir)?;
    let sshd = devcontainer::prepare_sshd(&exec.id, &user, &key)?;
    let host = RemoteHost {
        alias: format!("tod-{}", exec.name),
        user,
    };
    let entry = host_entry(
        &dir,
        &host,
        &devcontainer::sshd_proxy_command(&exec.id, &sshd.sshd),
    );
    update_file(
        &dir.join("config"),
        &host.alias,
        &entry,
        parse_config,
        render_config,
    )?;
    let known = format!("{} {}\n", host.alias, sshd.host_key);
    update_file(
        &dir.join("known_hosts"),
        &host.alias,
        &known,
        parse_known_hosts,
        |entries| entries.values().cloned().collect(),
    )?;
    Ok(host)
}

/// The container's default user, when it names no dev container user.
fn container_user(exec: &ContainerExec) -> Result<String> {
    let out = exec.output("/", "id", &["-un"])?;
    let user = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || user.is_empty() {
        bail!(
            "could not tell which user dev container `{}` runs as",
            exec.name
        );
    }
    Ok(user)
}

fn host_entry(dir: &Path, host: &RemoteHost, proxy_command: &str) -> String {
    format!(
        "Host {alias}\n  User {user}\n  HostKeyAlias {alias}\n  ProxyCommand {proxy_command}\n  \
         IdentityFile \"{key}\"\n  IdentitiesOnly yes\n  UserKnownHostsFile \"{known}\"\n  \
         StrictHostKeyChecking yes\n",
        alias = host.alias,
        user = host.user,
        key = config_path(&dir.join("id_ed25519")),
        known = config_path(&dir.join("known_hosts")),
    )
}

/// `config`'s `Host` entries by alias.
fn parse_config(text: &str) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(alias) = line.strip_prefix("Host ") {
            if let Some((alias, entry)) = current.take() {
                entries.insert(alias, entry);
            }
            current = Some((alias.trim().to_string(), String::new()));
        }
        if let Some((_, entry)) = &mut current {
            entry.push_str(line);
            entry.push('\n');
        }
    }
    if let Some((alias, entry)) = current {
        entries.insert(alias, entry);
    }
    entries
}

fn render_config(entries: &BTreeMap<String, String>) -> String {
    let mut text = CONFIG_HEADER.to_string();
    for entry in entries.values() {
        text.push('\n');
        text.push_str(entry.trim_end());
        text.push('\n');
    }
    text
}

/// `known_hosts` lines by host.
fn parse_known_hosts(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let host = line.split_whitespace().next()?;
            Some((host.to_string(), format!("{line}\n")))
        })
        .collect()
}

/// Replace `alias`'s entry in the file at `path`, keeping the others.
fn update_file(
    path: &Path,
    alias: &str,
    entry: &str,
    parse: fn(&str) -> BTreeMap<String, String>,
    render: impl Fn(&BTreeMap<String, String>) -> String,
) -> Result<()> {
    let mut entries = match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    if entries.get(alias).map(String::as_str) == Some(entry) {
        return Ok(());
    }
    entries.insert(alias.to_string(), entry.to_string());
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, render(&entries)).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}

/// Create `dir` readable by this user only. `ssh` refuses a key, and on
/// Windows even an included config file, that others can read.
fn ensure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    restrict_to_user(dir)
}

#[cfg(unix)]
fn restrict_to_user(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restrict {}", dir.display()))
}

/// Replace the directory's inherited access with full control for this user
/// alone, inherited by everything in it. Idempotent.
#[cfg(windows)]
fn restrict_to_user(dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let user = std::env::var("USERNAME").context("USERNAME is not set")?;
    let account = match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
        _ => user,
    };
    let out = Command::new("icacls")
        .arg(dir)
        .args([
            "/inheritance:r",
            "/grant:r",
            &format!("{account}:(OI)(CI)F"),
        ])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("run icacls")?;
    if !out.status.success() {
        bail!(
            "restrict {} to {account}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(())
}

/// tod's key, created on first use. Returns its public half.
fn ensure_key(dir: &Path) -> Result<String> {
    let key = dir.join("id_ed25519");
    let public = dir.join("id_ed25519.pub");
    if !key.is_file() || !public.is_file() {
        let _ = std::fs::remove_file(&key);
        let _ = std::fs::remove_file(&public);
        let mut command = Command::new(ssh_keygen());
        command
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "tod", "-f"])
            .arg(&key)
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let out = command
            .output()
            .context("run ssh-keygen (is OpenSSH installed?)")?;
        if !out.status.success() {
            bail!(
                "ssh-keygen: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    let text =
        std::fs::read_to_string(&public).with_context(|| format!("read {}", public.display()))?;
    Ok(text.trim().to_string())
}

/// Windows' own OpenSSH when it is there, else `ssh-keygen` on `PATH`.
fn ssh_keygen() -> PathBuf {
    if cfg!(windows) {
        if let Some(root) = std::env::var_os("SystemRoot") {
            let bundled = PathBuf::from(root)
                .join("System32")
                .join("OpenSSH")
                .join("ssh-keygen.exe");
            if bundled.is_file() {
                return bundled;
            }
        }
    }
    PathBuf::from("ssh-keygen")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_include_goes_on_top_once() {
        let dir = temp_dir("ssh-include");
        let data_root = dir.join("data root");
        let user_config = dir.join(".ssh").join("config");
        assert!(!has_include(&user_config, &data_root).unwrap());

        add_include(&user_config, &data_root).unwrap();
        assert!(has_include(&user_config, &data_root).unwrap());
        let text = std::fs::read_to_string(&user_config).unwrap();
        assert!(text.contains(&include_line(&data_root)), "{text}");

        std::fs::write(&user_config, "Host box\r\n  User me\r\n").unwrap();
        add_include(&user_config, &data_root).unwrap();
        add_include(&user_config, &data_root).unwrap();
        let text = std::fs::read_to_string(&user_config).unwrap();
        assert_eq!(text.matches("Include").count(), 1, "{text}");
        assert!(
            text.ends_with("\r\n\r\nHost box\r\n  User me\r\n"),
            "{text:?}"
        );
        let include = text.lines().position(|l| l.starts_with("Include"));
        assert_eq!(include, Some(1), "{text}");
    }

    #[test]
    fn an_include_is_recognized_however_it_is_written() {
        let dir = temp_dir("ssh-include-forms");
        let data_root = dir.join("root");
        let user_config = dir.join("config");
        let ours = include_paths(&data_root);
        let all = ours.join(" ");
        for line in [
            format!("Include {all}"),
            format!(
                "  include = {}",
                include_line(&data_root).trim_start_matches("Include ")
            ),
            format!("Include ~/.ssh/other {}", all.replace('/', "\\")),
        ] {
            std::fs::write(&user_config, format!("{line}\n")).unwrap();
            assert!(has_include(&user_config, &data_root).unwrap(), "{line}");
        }
        std::fs::write(&user_config, format!("# Include {all}\n")).unwrap();
        assert!(!has_include(&user_config, &data_root).unwrap());
    }

    #[test]
    fn an_older_include_is_replaced() {
        let dir = temp_dir("ssh-include-old");
        let data_root = dir.join("root");
        let user_config = dir.join("config");
        let first = &include_paths(&data_root)[0];
        std::fs::write(
            &user_config,
            format!("{INCLUDE_COMMENT}\nInclude \"{first}\"\n\nHost box\n  User me\n"),
        )
        .unwrap();
        add_include(&user_config, &data_root).unwrap();
        let text = std::fs::read_to_string(&user_config).unwrap();
        assert_eq!(
            text,
            format!(
                "{INCLUDE_COMMENT}\n{}\n\nHost box\n  User me\n",
                include_line(&data_root)
            )
        );
    }

    #[test]
    fn windows_includes_name_the_msys_form_too() {
        let paths = include_paths(Path::new("C:/data/root"));
        if cfg!(windows) {
            assert_eq!(
                paths,
                ["C:/data/root/ssh/config", "/c/data/root/ssh/config"]
            );
        } else {
            assert_eq!(paths.len(), 1);
        }
    }

    #[test]
    fn entries_are_replaced_by_alias() {
        let dir = temp_dir("ssh-entries");
        let config = dir.join("config");
        let host = |alias: &str| RemoteHost {
            alias: alias.into(),
            user: "vscode".into(),
        };
        let entry = |alias: &str, proxy: &str| host_entry(&dir, &host(alias), proxy);
        update_file(
            &config,
            "tod-a",
            &entry("tod-a", "one"),
            parse_config,
            render_config,
        )
        .unwrap();
        update_file(
            &config,
            "tod-b",
            &entry("tod-b", "two"),
            parse_config,
            render_config,
        )
        .unwrap();
        update_file(
            &config,
            "tod-a",
            &entry("tod-a", "three"),
            parse_config,
            render_config,
        )
        .unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.starts_with(CONFIG_HEADER));
        assert_eq!(text.matches("Host ").count(), 2, "{text}");
        assert!(text.contains("ProxyCommand three") && text.contains("ProxyCommand two"));
        assert!(!text.contains("ProxyCommand one"));
        assert_eq!(parse_config(&text).len(), 2);

        let known = dir.join("known_hosts");
        let render = |entries: &BTreeMap<String, String>| entries.values().cloned().collect();
        update_file(
            &known,
            "tod-a",
            "tod-a ssh-ed25519 K1\n",
            parse_known_hosts,
            render,
        )
        .unwrap();
        update_file(
            &known,
            "tod-a",
            "tod-a ssh-ed25519 K2\n",
            parse_known_hosts,
            render,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&known).unwrap(),
            "tod-a ssh-ed25519 K2\n"
        );
    }

    /// `ssh` logs in to a real container with nothing but tod's config.
    /// Needs `TOD_TEST_DEV_CONTAINER`: a running container with sshd.
    #[test]
    fn ssh_reaches_a_readied_container() {
        let Ok(container) = std::env::var("TOD_TEST_DEV_CONTAINER") else {
            eprintln!("skipped: TOD_TEST_DEV_CONTAINER is not set");
            return;
        };
        let dir = temp_dir("ssh-live");
        let data_root = dir.join("data root");
        let user_config = dir.join("user-config");
        add_include(&user_config, &data_root).unwrap();
        // Twice: readying again changes nothing.
        connect_container(&data_root, &container).unwrap();
        let host = connect_container(&data_root, &container).unwrap();
        let started = std::time::Instant::now();
        // Windows' own OpenSSH, as editors run it: Git's msys `ssh` does not
        // read an `Include` of a drive-letter path.
        let ssh = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join(r"System32\OpenSSH\ssh.exe"))
            .filter(|ssh| cfg!(windows) && ssh.is_file())
            .unwrap_or_else(|| PathBuf::from("ssh"));
        let out = Command::new(ssh)
            .arg("-F")
            .arg(&user_config)
            .args(["-o", "BatchMode=yes", &host.alias, "id", "-un"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), host.user);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "login took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn config_paths_use_forward_slashes() {
        assert_eq!(config_path(Path::new(r"\\?\C:\data\root")), "C:/data/root");
        assert_eq!(config_path(Path::new("/home/me/root")), "/home/me/root");
    }
}
