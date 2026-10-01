//! Keeping an interactive cloud sandbox's Claude session logs across the
//! sandbox being recreated (a changed credential, a lost sandbox).
//!
//! Claude writes each session to
//! `<claude dir>/projects/<project>/<session id>.jsonl` (`<claude dir>` is
//! `$CLAUDE_CONFIG_DIR`, else `~/.claude`), where `<project>` is the agent's
//! working directory with every character that is not a letter or digit
//! replaced by `-` ([`project_dir_name`]). A conversation resumes by that id,
//! so a new sandbox that holds the same file resumes the same session.
//!
//! The app copies each log out of the sandbox (`pull`, appending complete
//! lines only, at the offset the copy already has) into
//! `<data root>/sandbox-sessions/<node>/<project>__<id>.jsonl`, and copies
//! them back into a sandbox it makes (`restore`). The project directory name
//! is kept as recorded, so the log lands where Claude looks for it whatever
//! the directory encoding. Autonomous nodes mirror theirs from the
//! supervisor (`tod-supervisor`'s transcripts); this is the interactive
//! counterpart, driven by the app.
//!
//! Everything here blocks on the network: never on the UI thread.

use super::sandbox::SandboxExec;
use anyhow::{Context, Result, bail};
use base64::Engine;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SEPARATOR: &str = "__";

/// Pulls and restores never overlap: a pull appends at the copy's length.
static LOCK: Mutex<()> = Mutex::new(());

/// Claude's project directory name for a working directory: every character
/// that is not an ASCII letter or digit becomes `-`
/// (`/workspace/repo` -> `-workspace-repo`).
pub fn project_dir_name(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Where a node's copies are kept.
pub fn local_dir(data_root: &Path, node_id: &str) -> PathBuf {
    data_root.join("sandbox-sessions").join(node_id)
}

fn safe(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 150
        && !part.starts_with('.')
        && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn file_name(project: &str, session: &str) -> Option<String> {
    (safe(project) && safe(session) && !project.contains(SEPARATOR) && !session.contains(".."))
        .then(|| format!("{project}{SEPARATOR}{session}.jsonl"))
}

fn split_file_name(name: &str) -> Option<(String, String)> {
    let stem = name.strip_suffix(".jsonl")?;
    let (project, session) = stem.split_once(SEPARATOR)?;
    file_name(project, session).map(|_| (project.to_string(), session.to_string()))
}

/// One session log in a sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLog {
    pub project: String,
    pub session: String,
    pub size: u64,
}

/// Where a sandbox's logs are read and written.
pub trait Remote {
    fn list(&self) -> Result<Vec<RemoteLog>>;
    /// The log's bytes from `offset` on.
    fn read_from(&self, project: &str, session: &str, offset: u64) -> Result<Vec<u8>>;
    /// Replaces the log.
    fn write(&self, project: &str, session: &str, bytes: &[u8]) -> Result<()>;
}

/// Copies what each sandbox log has beyond its local copy. The bytes copied.
pub fn pull(remote: &dyn Remote, local: &Path) -> Result<u64> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut copied = 0;
    for log in remote.list()? {
        let Some(name) = file_name(&log.project, &log.session) else { continue };
        let path = local.join(name);
        let have = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if log.size <= have {
            continue;
        }
        let mut bytes = remote.read_from(&log.project, &log.session, have)?;
        // Only complete lines: a line still being written is taken next time.
        match bytes.iter().rposition(|b| *b == b'\n') {
            Some(end) => bytes.truncate(end + 1),
            None => continue,
        }
        std::fs::create_dir_all(local)?;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(&bytes)?;
        copied += bytes.len() as u64;
    }
    Ok(copied)
}

/// Copies each local log the sandbox lacks, or has shorter, back into it.
/// The number restored. Nothing local is nothing to do (and no network call).
pub fn restore(remote: &dyn Remote, local: &Path) -> Result<usize> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut wanted = Vec::new();
    if let Ok(entries) = std::fs::read_dir(local) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((project, session)) = split_file_name(&name) {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if size > 0 {
                    wanted.push((project, session, size, entry.path()));
                }
            }
        }
    }
    if wanted.is_empty() {
        return Ok(0);
    }
    let there = remote.list()?;
    let mut restored = 0;
    for (project, session, size, path) in wanted {
        let held = there
            .iter()
            .find(|l| l.project == project && l.session == session)
            .map_or(0, |l| l.size);
        if held >= size {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        remote.write(&project, &session, &bytes)?;
        restored += 1;
    }
    Ok(restored)
}

/// A sandbox's logs, through its relay.
pub struct SandboxRemote {
    exec: SandboxExec,
}

impl SandboxRemote {
    pub fn new(sandbox: &str) -> Self {
        Self { exec: SandboxExec::new(sandbox) }
    }

    fn sh(&self, script: &str, args: &[&str]) -> Result<Vec<u8>> {
        let mut all = vec!["-c", script, "sh"];
        all.extend_from_slice(args);
        let out = self.exec.output("/", "sh", &all)?;
        if !out.status.success() {
            bail!(
                "in sandbox {}: {}",
                self.exec.name,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }
}

const PROJECTS: &str = r#"d="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects""#;
/// The most raw bytes sent in one command (base64 makes it a third more, and
/// one argument is limited to 128 KiB).
#[cfg(not(windows))]
const CHUNK: usize = 48 * 1024;
/// A Windows command line is limited to 32 K characters in all.
#[cfg(windows)]
const CHUNK: usize = 16 * 1024;

fn list_script() -> String {
    format!(r#"{PROJECTS}; for f in "$d"/*/*.jsonl; do [ -f "$f" ] && echo "$(wc -c < "$f") $f"; done; exit 0"#)
}

/// `size path` lines from [`list_script`].
fn parse_listing(out: &str) -> Vec<RemoteLog> {
    out.lines()
        .filter_map(|line| {
            let (size, path) = line.trim().split_once(' ')?;
            let size: u64 = size.trim().parse().ok()?;
            let mut parts = path.trim().rsplit('/');
            let session = parts.next()?.strip_suffix(".jsonl")?;
            let project = parts.next()?;
            file_name(project, session)?;
            Some(RemoteLog { project: project.into(), session: session.into(), size })
        })
        .collect()
}

type Run<'a> = &'a dyn Fn(&str, &[&str]) -> Result<Vec<u8>>;

fn sh_list(run: Run) -> Result<Vec<RemoteLog>> {
    let out = run(&list_script(), &[])?;
    Ok(parse_listing(&String::from_utf8_lossy(&out)))
}

fn sh_read_from(run: Run, project: &str, session: &str, offset: u64) -> Result<Vec<u8>> {
    let script = format!(r#"{PROJECTS}; tail -c +"$(( $1 + 1 ))" "$d/$2/$3.jsonl""#);
    run(&script, &[&offset.to_string(), project, session])
}

fn sh_write(run: Run, project: &str, session: &str, bytes: &[u8]) -> Result<()> {
    // Into a file of its own first, so a cut-off copy is never a log.
    for (n, chunk) in bytes.chunks(CHUNK).enumerate() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        let redirect = if n == 0 { ">" } else { ">>" };
        let script = format!(
            r#"{PROJECTS}; mkdir -p "$d/$1" && printf %s "$3" | base64 -d {redirect} "$d/$1/$2.jsonl.restoring""#
        );
        run(&script, &[project, session, &encoded])?;
    }
    let script = format!(
        r#"{PROJECTS}; if [ -f "$d/$1/$2.jsonl.restoring" ]; then mv "$d/$1/$2.jsonl.restoring" "$d/$1/$2.jsonl"; else : > "$d/$1/$2.jsonl"; fi"#
    );
    run(&script, &[project, session])?;
    Ok(())
}

impl Remote for SandboxRemote {
    fn list(&self) -> Result<Vec<RemoteLog>> {
        sh_list(&|script, args| self.sh(script, args))
    }

    fn read_from(&self, project: &str, session: &str, offset: u64) -> Result<Vec<u8>> {
        sh_read_from(&|script, args| self.sh(script, args), project, session, offset)
    }

    fn write(&self, project: &str, session: &str, bytes: &[u8]) -> Result<()> {
        sh_write(&|script, args| self.sh(script, args), project, session, bytes)
    }
}

/// A running dev container's logs, as the container's user sees them.
pub struct ContainerRemote {
    exec: tod_agent::devcontainer::ContainerExec,
}

impl ContainerRemote {
    pub fn new(container: &str) -> Result<Self> {
        Ok(Self { exec: tod_agent::devcontainer::ContainerExec::connect(container)? })
    }

    fn sh(&self, script: &str, args: &[&str]) -> Result<Vec<u8>> {
        let mut all = vec!["-c", script, "sh"];
        all.extend_from_slice(args);
        let out = self.exec.output("/", "sh", &all)?;
        if !out.status.success() {
            bail!("in container {}: {}", self.exec.name, String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(out.stdout)
    }
}

impl Remote for ContainerRemote {
    fn list(&self) -> Result<Vec<RemoteLog>> {
        sh_list(&|script, args| self.sh(script, args))
    }

    fn read_from(&self, project: &str, session: &str, offset: u64) -> Result<Vec<u8>> {
        sh_read_from(&|script, args| self.sh(script, args), project, session, offset)
    }

    fn write(&self, project: &str, session: &str, bytes: &[u8]) -> Result<()> {
        sh_write(&|script, args| self.sh(script, args), project, session, bytes)
    }
}

/// This machine's logs, under a Claude config directory (`CLAUDE_CONFIG_DIR`,
/// else `~/.claude`) or one given.
pub struct HostRemote {
    projects: PathBuf,
}

impl HostRemote {
    pub fn new() -> Result<Self> {
        let base = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => dirs::home_dir().context("no home directory")?.join(".claude"),
        };
        Ok(Self::at(base))
    }

    /// Logs under `<claude_dir>/projects`.
    pub fn at(claude_dir: impl Into<PathBuf>) -> Self {
        Self { projects: claude_dir.into().join("projects") }
    }

    fn path(&self, project: &str, session: &str) -> Result<PathBuf> {
        file_name(project, session).context("not a session log name")?;
        Ok(self.projects.join(project).join(format!("{session}.jsonl")))
    }
}

impl Remote for HostRemote {
    fn list(&self) -> Result<Vec<RemoteLog>> {
        let mut logs = Vec::new();
        let Ok(projects) = std::fs::read_dir(&self.projects) else { return Ok(logs) };
        for project in projects.flatten() {
            let Ok(files) = std::fs::read_dir(project.path()) else { continue };
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                let Some(session) = name.strip_suffix(".jsonl") else { continue };
                let project = project.file_name().to_string_lossy().into_owned();
                if file_name(&project, session).is_none() {
                    continue;
                }
                let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                logs.push(RemoteLog { project, session: session.to_string(), size });
            }
        }
        Ok(logs)
    }

    fn read_from(&self, project: &str, session: &str, offset: u64) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let path = self.path(project, session)?;
        let mut file = std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn write(&self, project: &str, session: &str, bytes: &[u8]) -> Result<()> {
        let path = self.path(project, session)?;
        let dir = path.parent().context("no project directory")?;
        std::fs::create_dir_all(dir)?;
        // Into a file of its own first, so a cut-off copy is never a log.
        let tmp = path.with_extension("jsonl.restoring");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// A node's mirror directory (`<data root>/sandbox-sessions/<node>/`, files
/// `<project>__<session>.jsonl`) as a place logs are read from.
pub struct MirrorRemote {
    dir: PathBuf,
}

impl MirrorRemote {
    pub fn new(data_root: &Path, node_id: &str) -> Self {
        Self { dir: local_dir(data_root, node_id) }
    }
}

impl Remote for MirrorRemote {
    fn list(&self) -> Result<Vec<RemoteLog>> {
        let mut logs = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Ok(logs) };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((project, session)) = split_file_name(&name) {
                logs.push(RemoteLog { project, session, size: entry.metadata().map(|m| m.len()).unwrap_or(0) });
            }
        }
        Ok(logs)
    }

    fn read_from(&self, project: &str, session: &str, offset: u64) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let name = file_name(project, session).context("not a session log name")?;
        let mut file = std::fs::File::open(self.dir.join(name))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn write(&self, _: &str, _: &str, _: &[u8]) -> Result<()> {
        bail!("the mirror is only read through this")
    }
}

/// Make sure `target` holds `session`'s newest log under `to_project`,
/// copying it from whichever of `sources` has the longest one. `true` when it
/// is there after. A log only grows, so the longest is the newest: the
/// target's own copy from an earlier visit is replaced when another place
/// has since added to the session, and never when it is the longest.
pub fn ensure_session(
    target: &dyn Remote,
    sources: &[&dyn Remote],
    session: &str,
    to_project: &str,
) -> Result<bool> {
    let held = target
        .list()?
        .iter()
        .filter(|l| l.session == session && l.project == to_project)
        .map(|l| l.size)
        .max()
        .unwrap_or(0);
    let mut best: Option<(&dyn Remote, u64)> = None;
    for source in sources {
        match source.list() {
            Ok(logs) => {
                let size = logs.iter().filter(|l| l.session == session).map(|l| l.size).max().unwrap_or(0);
                if size > best.map_or(0, |(_, s)| s) {
                    best = Some((*source, size));
                }
            }
            Err(err) => tracing::warn!("listing logs for session {session}: {err:#}"),
        }
    }
    if let Some((source, size)) = best {
        if size > held {
            match transfer(source, target, session, to_project) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(err) => tracing::warn!("copying session {session}: {err:#}"),
            }
        }
    }
    Ok(held > 0)
}

/// Move one session's log from one environment to another for a node that is
/// moving (`doc/agentd.md`, "Moving a node"): the whole log, complete lines
/// only, written where `to_project` (the target's working directory, as
/// [`project_dir_name`]) makes Claude look for it, so the same session id
/// resumes there. `false` when `from` has no log for `session`. The copy
/// there is replaced only if `from`'s is longer.
pub fn transfer(from: &dyn Remote, to: &dyn Remote, session: &str, to_project: &str) -> Result<bool> {
    let Some(log) = from.list()?.into_iter().filter(|l| l.session == session).max_by_key(|l| l.size) else {
        return Ok(false);
    };
    let held = to
        .list()?
        .into_iter()
        .find(|l| l.session == session && l.project == to_project)
        .map_or(0, |l| l.size);
    if held >= log.size {
        return Ok(true);
    }
    let mut bytes = from.read_from(&log.project, session, 0)?;
    match bytes.iter().rposition(|b| *b == b'\n') {
        Some(end) => bytes.truncate(end + 1),
        None => return Ok(false),
    }
    to.write(to_project, session, &bytes)?;
    Ok(true)
}

/// Copy `sandbox`'s session logs into `node_id`'s local copies.
pub fn pull_node(data_root: &Path, node_id: &str, sandbox: &str) -> Result<u64> {
    pull(&SandboxRemote::new(sandbox), &local_dir(data_root, node_id))
}

/// Copy `node_id`'s local session logs into `sandbox`.
pub fn restore_node(data_root: &Path, node_id: &str, sandbox: &str) -> Result<usize> {
    let local = local_dir(data_root, node_id);
    if !local.is_dir() {
        return Ok(0);
    }
    restore(&SandboxRemote::new(sandbox), &local)
}

/// [`pull_node`] on a thread of its own (after a turn), logging a failure.
pub fn pull_node_in_background(data_root: PathBuf, node_id: String, sandbox: String) {
    let spawned = std::thread::Builder::new().name("tod-session-log".into()).spawn(move || {
        if let Err(err) = pull_node(&data_root, &node_id, &sandbox) {
            tracing::warn!("keeping {sandbox}'s session logs: {err:#}");
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("the session log copy did not start: {err}");
    }
}

/// Copy what `remote` has beyond the mirror for the one `session` (not every
/// log: a machine's Claude directory holds the user's other work too).
pub fn keep_session(remote: &dyn Remote, local: &Path, session: &str) -> Result<u64> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut copied = 0;
    for log in remote.list()?.into_iter().filter(|l| l.session == session) {
        let Some(name) = file_name(&log.project, &log.session) else { continue };
        let path = local.join(name);
        let have = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if log.size <= have {
            continue;
        }
        let mut bytes = remote.read_from(&log.project, &log.session, have)?;
        match bytes.iter().rposition(|b| *b == b'\n') {
            Some(end) => bytes.truncate(end + 1),
            None => continue,
        }
        std::fs::create_dir_all(local)?;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(&bytes)?;
        copied += bytes.len() as u64;
    }
    Ok(copied)
}

/// [`keep_session`] for the session of a conversation that ran in `workdir`,
/// on a thread of its own, logging a failure. A sandbox's logs are kept by
/// [`pull_node_in_background`].
pub fn keep_session_in_background(
    data_root: PathBuf,
    node_id: String,
    session: String,
    workdir: super::workdir::Workdir,
) {
    let spawned = std::thread::Builder::new().name("tod-session-log".into()).spawn(move || {
        let local = local_dir(&data_root, &node_id);
        let result = match &workdir {
            super::workdir::Workdir::Host(_) => {
                HostRemote::new().and_then(|remote| keep_session(&remote, &local, &session))
            }
            super::workdir::Workdir::Container { container, .. } => {
                ContainerRemote::new(container).and_then(|remote| keep_session(&remote, &local, &session))
            }
            super::workdir::Workdir::Sandbox { .. } => return,
        };
        if let Err(err) = result {
            tracing::warn!("keeping session {session}'s log: {err:#}");
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("the session log copy did not start: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sandbox's projects directory, on disk.
    struct Fs(PathBuf);

    impl Fs {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tod-session-log-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn put(&self, project: &str, session: &str, text: &str) {
            let dir = self.0.join(project);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{session}.jsonl")), text).unwrap();
        }
        fn get(&self, project: &str, session: &str) -> Option<String> {
            std::fs::read_to_string(self.0.join(project).join(format!("{session}.jsonl"))).ok()
        }
    }

    impl Remote for Fs {
        fn list(&self) -> Result<Vec<RemoteLog>> {
            let mut out = Vec::new();
            for project in std::fs::read_dir(&self.0)?.flatten() {
                for file in std::fs::read_dir(project.path())?.flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if let Some(session) = name.strip_suffix(".jsonl") {
                        out.push(RemoteLog {
                            project: project.file_name().to_string_lossy().into_owned(),
                            session: session.into(),
                            size: file.metadata()?.len(),
                        });
                    }
                }
            }
            Ok(out)
        }
        fn read_from(&self, project: &str, session: &str, offset: u64) -> Result<Vec<u8>> {
            let all = std::fs::read(self.0.join(project).join(format!("{session}.jsonl")))?;
            Ok(all[offset as usize..].to_vec())
        }
        fn write(&self, project: &str, session: &str, bytes: &[u8]) -> Result<()> {
            let dir = self.0.join(project);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join(format!("{session}.jsonl")), bytes)?;
            Ok(())
        }
    }

    #[test]
    fn the_project_directory_is_the_cwd_with_non_alphanumerics_as_dashes() {
        assert_eq!(project_dir_name("/workspace/repo"), "-workspace-repo");
        assert_eq!(project_dir_name("/root/my.app_v2"), "-root-my-app-v2");
        assert_eq!(project_dir_name("/"), "-");
    }

    #[test]
    fn names_that_could_leave_the_directory_are_refused() {
        assert!(file_name("-workspace-repo", "0a1b-22").is_some());
        assert!(file_name("..", "x").is_none());
        assert!(file_name("p", "../x").is_none());
        assert!(file_name("a/b", "x").is_none());
        assert!(file_name("a__b", "x").is_none());
        assert_eq!(
            split_file_name("-workspace-repo__0a1b.jsonl"),
            Some(("-workspace-repo".into(), "0a1b".into()))
        );
        assert_eq!(split_file_name("x.jsonl"), None);
    }

    #[test]
    fn a_listing_is_parsed_and_unsafe_paths_are_dropped() {
        let out = "120 /root/.claude/projects/-workspace-repo/abc.jsonl\n  7 /root/.claude/projects/-w/x y.jsonl\nnope\n";
        assert_eq!(
            parse_listing(out),
            [RemoteLog { project: "-workspace-repo".into(), session: "abc".into(), size: 120 }]
        );
    }

    #[test]
    fn a_pull_copies_complete_lines_and_continues_where_it_stopped() {
        let (sandbox, local) = (Fs::new("remote"), Fs::new("local"));
        sandbox.put("-workspace-repo", "s1", "{\"a\":1}\n{\"b\":2}\n{\"c\"");
        assert_eq!(pull(&sandbox, &local.0).unwrap(), 16);
        let copy = local.0.join("-workspace-repo__s1.jsonl");
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "{\"a\":1}\n{\"b\":2}\n");
        sandbox.put("-workspace-repo", "s1", "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n");
        assert_eq!(pull(&sandbox, &local.0).unwrap(), 8);
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n");
        assert_eq!(pull(&sandbox, &local.0).unwrap(), 0, "nothing new");
    }

    #[test]
    fn a_new_sandbox_gets_the_log_back_where_claude_looks_for_it() {
        let (old, local) = (Fs::new("old"), Fs::new("local"));
        old.put("-workspace-repo", "s1", "{\"a\":1}\n{\"b\":2}\n");
        pull(&old, &local.0).unwrap();
        let fresh = Fs::new("fresh");
        assert_eq!(restore(&fresh, &local.0).unwrap(), 1);
        assert_eq!(fresh.get("-workspace-repo", "s1").as_deref(), Some("{\"a\":1}\n{\"b\":2}\n"));
        assert_eq!(restore(&fresh, &local.0).unwrap(), 0, "already there");
    }

    #[test]
    fn a_restore_never_replaces_a_longer_log_in_the_sandbox() {
        let (sandbox, local) = (Fs::new("remote"), Fs::new("local"));
        std::fs::write(local.0.join("p__s1.jsonl"), "{\"a\":1}\n").unwrap();
        sandbox.put("p", "s1", "{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(restore(&sandbox, &local.0).unwrap(), 0);
        assert_eq!(sandbox.get("p", "s1").as_deref(), Some("{\"a\":1}\n{\"b\":2}\n"));
    }

    #[test]
    fn nothing_saved_means_nothing_to_restore_and_no_call() {
        struct Never;
        impl Remote for Never {
            fn list(&self) -> Result<Vec<RemoteLog>> {
                panic!("not called")
            }
            fn read_from(&self, _: &str, _: &str, _: u64) -> Result<Vec<u8>> {
                panic!("not called")
            }
            fn write(&self, _: &str, _: &str, _: &[u8]) -> Result<()> {
                panic!("not called")
            }
        }
        let local = Fs::new("empty");
        assert_eq!(restore(&Never, &local.0).unwrap(), 0);
        assert_eq!(restore(&Never, &local.0.join("missing")).unwrap(), 0);
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;

    #[test]
    fn a_session_moves_under_the_target_working_directory() {
        let dir = std::env::temp_dir().join(format!("tod-session-transfer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let from = HostRemote::at(dir.join("a"));
        let to = HostRemote::at(dir.join("b"));
        let id = "11111111-2222-3333-4444-555555555555";
        let source = project_dir_name("/home/me/repo/.worktrees/task-x");
        let target = project_dir_name("/workspace/repo/.worktrees/task-x");
        // The last line is still being written, so it does not travel.
        from.write(&source, id, b"{\"a\":1}
{\"b\":2}
{\"c\"").unwrap();

        assert!(transfer(&from, &to, id, &target).unwrap());
        let logs = to.list().unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].project, target);
        assert_eq!(to.read_from(&target, id, 0).unwrap(), b"{\"a\":1}
{\"b\":2}
");

        // Moving again with nothing new is a no-op that still succeeds.
        assert!(transfer(&from, &to, id, &target).unwrap());
        // A session the source does not have is reported, not invented.
        assert!(!transfer(&from, &to, "no-such-session", &target).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keep_session_copies_only_that_session_and_only_whole_lines() {
        let dir = std::env::temp_dir().join(format!("tod-session-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let host = HostRemote::at(dir.join("claude"));
        let id = "cccccccc-2222-3333-4444-555555555555";
        host.write("-p", id, b"{\"a\":1}
{\"b\"").unwrap();
        host.write("-p", "dddddddd-2222-3333-4444-555555555555", b"{\"other\":1}
").unwrap();
        let mirror = dir.join("mirror");
        assert_eq!(keep_session(&host, &mirror, id).unwrap(), 8);
        let kept = MirrorRemote { dir: mirror.clone() };
        assert_eq!(kept.list().unwrap().len(), 1);
        // The rest arrives once the line is whole.
        host.write("-p", id, b"{\"a\":1}
{\"b\":2}
").unwrap();
        keep_session(&host, &mirror, id).unwrap();
        assert_eq!(kept.read_from("-p", id, 0).unwrap(), b"{\"a\":1}
{\"b\":2}
");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_session_copies_only_what_the_target_lacks() {
        let dir = std::env::temp_dir().join(format!("tod-session-ensure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let from = HostRemote::at(dir.join("a"));
        let to = HostRemote::at(dir.join("b"));
        let id = "aaaaaaaa-2222-3333-4444-555555555555";
        from.write("-old", id, b"{\"x\":1}
").unwrap();
        assert!(ensure_session(&to, &[&from], id, "-new").unwrap());
        assert_eq!(to.read_from("-new", id, 0).unwrap(), b"{\"x\":1}
");
        // The target's own, longer log is not replaced.
        to.write("-new", id, b"{\"x\":1}
{\"y\":2}
").unwrap();
        assert!(ensure_session(&to, &[&from], id, "-new").unwrap());
        assert_eq!(to.read_from("-new", id, 0).unwrap(), b"{\"x\":1}
{\"y\":2}
");
        assert!(!ensure_session(&to, &[&from], "nothing", "-new").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shell scripts the sandbox and container remotes share, run by a
    /// local `sh` against a throwaway Claude directory.
    #[test]
    fn the_shell_remote_scripts_round_trip_a_log() {
        let Ok(probe) = std::process::Command::new("sh").arg("-c").arg("command -v base64").output() else { return };
        if !probe.status.success() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("tod-session-sh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let claude = dir.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
        let run = |script: &str, args: &[&str]| -> Result<Vec<u8>> {
            let mut all = vec!["-c", script, "sh"];
            all.extend_from_slice(args);
            let out = std::process::Command::new("sh").args(all).env("CLAUDE_CONFIG_DIR", &claude).output()?;
            anyhow::ensure!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            Ok(out.stdout)
        };
        let id = "bbbbbbbb-2222-3333-4444-555555555555";
        let body = b"{\"a\":1}
{\"b\":2}
".repeat(1000);
        sh_write(&run, "-proj", id, &body).unwrap();
        let logs = sh_list(&run).unwrap();
        assert_eq!(logs, vec![RemoteLog { project: "-proj".into(), session: id.into(), size: body.len() as u64 }]);
        assert_eq!(sh_read_from(&run, "-proj", id, 8).unwrap(), body[8..].to_vec());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The move matrix's session-log leg against a real place: a log goes
    /// host → place under the place's own project name, and comes back
    /// unchanged. `place` is the remote under test.
    fn round_trip_through(place: &dyn Remote, tag: &str) {
        let dir = std::env::temp_dir().join(format!("tod-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let host = HostRemote::at(&dir);
        let id = format!("cccccccc-{}-3333-4444-555555555555", &format!("{:04x}", std::process::id() & 0xffff));
        let body = b"{\"type\":\"user\",\"n\":1}
{\"type\":\"assistant\",\"n\":2}
".repeat(3000);
        host.write("-from-host", &id, &body).unwrap();
        // host -> place
        assert!(transfer(&host, place, &id, "-in-place").unwrap(), "nothing moved to the {tag}");
        let logs = place.list().unwrap();
        assert!(
            logs.iter().any(|l| l.project == "-in-place" && l.session == id && l.size == body.len() as u64),
            "the {tag} does not list the log: {logs:?}"
        );
        // place -> host, under a third name
        assert!(transfer(place, &host, &id, "-back-home").unwrap());
        assert_eq!(host.read_from("-back-home", &id, 0).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_log_round_trips_through_a_real_dev_container() {
        let Ok(container) = std::env::var("TOD_TEST_DEV_CONTAINER") else {
            eprintln!("skipped: set TOD_TEST_DEV_CONTAINER");
            return;
        };
        round_trip_through(&ContainerRemote::new(&container).unwrap(), "container");
    }

    /// Needs `TOD_TEST_SANDBOX` and `TOD_TEST_SANDBOX_ROOT`, as `fleet::sandbox`'s smoke test does.
    #[test]
    fn a_log_round_trips_through_a_real_cloud_sandbox() {
        let (Ok(name), Ok(root)) = (std::env::var("TOD_TEST_SANDBOX"), std::env::var("TOD_TEST_SANDBOX_ROOT")) else {
            eprintln!("skipped: set TOD_TEST_SANDBOX and TOD_TEST_SANDBOX_ROOT");
            return;
        };
        crate::fleet::sandbox::set_data_root(&std::fs::canonicalize(root).unwrap());
        round_trip_through(&SandboxRemote::new(&name), "sandbox");
    }

    /// One turn in `place`: the agent adds a line to its log there.
    fn take_turn(place: &dyn Remote, project: &str, session: &str, n: usize) {
        let mut log = place.read_from(project, session, 0).unwrap_or_default();
        log.extend_from_slice(format!("{{\"turn\":{n}}}
").as_bytes());
        place.write(project, session, &log).unwrap();
    }

    fn turns_in(place: &dyn Remote, project: &str, session: &str) -> usize {
        place.read_from(project, session, 0).map_or(0, |b| b.iter().filter(|c| **c == b'\n').count())
    }

    /// The move matrix for one session as the driver does it: before each
    /// turn the log is brought to where the node now runs (the longest of the
    /// host's and the mirror's), the turn adds a line, and the log is kept in
    /// the mirror. The session must never lose a turn on any hop, in
    /// particular a return to a place it already visited.
    fn run_move_matrix(host: &dyn Remote, others: &[(&str, &dyn Remote, &str)]) {
        let dir = std::env::temp_dir().join(format!("tod-session-matrix-{}", uuid::Uuid::new_v4()));
        let mirror_dir = local_dir(&dir, "node");
        let session = "dddddddd-2222-3333-4444-555555555555";
        let host_project = "-on-host";
        let mut turns = 0;
        // The route: host, each other place, host again, each place again.
        let mut route: Vec<(&str, &dyn Remote, &str)> = vec![("host", host, host_project)];
        route.extend(others.iter().copied());
        route.push(("host", host, host_project));
        route.extend(others.iter().copied());
        route.push(("host", host, host_project));
        for (name, place, project) in route {
            let mirror = MirrorRemote::new(&dir, "node");
            let mut sources: Vec<&dyn Remote> = Vec::new();
            if name != "host" {
                sources.push(host);
            }
            sources.push(&mirror);
            if turns > 0 {
                assert!(ensure_session(place, &sources, session, project).unwrap(), "no log reached the {name}");
            }
            assert_eq!(turns_in(place, project, session), turns, "the {name} resumes with a log of {turns} turns");
            turns += 1;
            take_turn(place, project, session, turns);
            keep_session(place, &mirror_dir, session).unwrap();
            assert_eq!(
                MirrorRemote::new(&dir, "node").read_from(project, session, 0).unwrap().iter().filter(|c| **c == b'\n').count(),
                turns,
                "the mirror keeps the {name}'s turn"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_loses_no_turn_moving_between_places_and_back() {
        let dir = std::env::temp_dir().join(format!("tod-session-matrix-places-{}", uuid::Uuid::new_v4()));
        let host = HostRemote::at(dir.join("host"));
        let a = HostRemote::at(dir.join("a"));
        let b = HostRemote::at(dir.join("b"));
        run_move_matrix(&host, &[("a", &a, "-in-a"), ("b", &b, "-in-b")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_move_across_a_real_container_and_a_real_sandbox_loses_no_turn() {
        let (Ok(container), Ok(name), Ok(root)) = (
            std::env::var("TOD_TEST_DEV_CONTAINER"),
            std::env::var("TOD_TEST_SANDBOX"),
            std::env::var("TOD_TEST_SANDBOX_ROOT"),
        ) else {
            eprintln!("skipped: set TOD_TEST_DEV_CONTAINER, TOD_TEST_SANDBOX, TOD_TEST_SANDBOX_ROOT");
            return;
        };
        crate::fleet::sandbox::set_data_root(&std::fs::canonicalize(root).unwrap());
        let dir = std::env::temp_dir().join(format!("tod-session-matrix-real-{}", uuid::Uuid::new_v4()));
        let host = HostRemote::at(&dir);
        let c = ContainerRemote::new(&container).unwrap();
        let s = SandboxRemote::new(&name);
        run_move_matrix(&host, &[("container", &c, "-in-container"), ("sandbox", &s, "-in-sandbox")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One `claude -p` turn with `args`, run by `run` (which knows where),
    /// returning the reply. The token is `TOD_TEST_CLAUDE_TOKEN`, given to
    /// Claude only as `CLAUDE_CODE_OAUTH_TOKEN`.
    fn claude_turn(
        run: &dyn Fn(&[String]) -> Result<std::process::Output>,
        token: &str,
        prompt: &str,
        session_arg: [&str; 2],
    ) -> String {
        let args: Vec<String> = ["env".to_string(), format!("CLAUDE_CODE_OAUTH_TOKEN={token}"), "claude".into()]
            .into_iter()
            .chain(["-p", prompt, session_arg[0], session_arg[1], "--output-format", "json"].map(String::from))
            .collect();
        let out = run(&args).unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let reply: serde_json::Value = serde_json::from_str(text.trim())
            .unwrap_or_else(|_| panic!("claude said: {text} {}", String::from_utf8_lossy(&out.stderr).replace(token, "<token>")));
        assert_eq!(reply["is_error"], false, "claude failed: {reply}");
        reply["result"].as_str().unwrap_or_default().to_string()
    }

    /// A real Claude session started on this machine, resumed in `place`
    /// from the copied log, then resumed here again: each side must know what
    /// the other was told. `run_there` runs a command in `place` at `cwd`.
    fn real_claude_move(
        place: &dyn Remote,
        cwd: &str,
        run_there: &dyn Fn(&[String]) -> Result<std::process::Output>,
        token: &str,
    ) {
        let dir = std::env::temp_dir().join(format!("tod-real-claude-{}", uuid::Uuid::new_v4()));
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let cfg = dir.join("cfg");
        let host = HostRemote::at(&cfg);
        let host_project = project_dir_name(&work.to_string_lossy());
        let id = uuid::Uuid::new_v4().to_string();
        let on_host = |prompt: &str, session_arg: [&str; 2]| -> String {
            let program = if cfg!(windows) { "claude.cmd" } else { "claude" };
            let out = std::process::Command::new(program)
                .args(["-p", prompt, session_arg[0], session_arg[1], "--output-format", "json"])
                .current_dir(&work)
                .env("CLAUDE_CONFIG_DIR", &cfg)
                .env("CLAUDE_CODE_OAUTH_TOKEN", token)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let reply: serde_json::Value = serde_json::from_str(text.trim()).unwrap_or_else(|_| {
                panic!("claude said: {text} {}", String::from_utf8_lossy(&out.stderr).replace(token, "<token>"))
            });
            assert_eq!(reply["is_error"], false, "claude failed: {reply}");
            reply["result"].as_str().unwrap_or_default().to_string()
        };

        on_host("Remember the secret word PINEAPPLE. Reply with just OK.", ["--session-id", &id]);
        assert!(host.read_from(&host_project, &id, 0).unwrap().len() > 0, "no log on the host");

        // To the place, under its own working directory's project name.
        let there = project_dir_name(cwd);
        assert!(transfer(&host, place, &id, &there).unwrap());
        let reply = claude_turn(
            run_there,
            token,
            "What secret word did I ask you to remember? Then also remember BANANA. Reply with the first word, then OK.",
            ["--resume", &id],
        );
        assert!(reply.to_uppercase().contains("PINEAPPLE"), "in the place, claude forgot the word: {reply}");

        // Back here: the host has not seen that turn until the log comes back.
        assert!(transfer(place, &host, &id, &host_project).unwrap());
        let reply = on_host("List every secret word I asked you to remember, in capitals, separated by commas.", ["--resume", &id]);
        let upper = reply.to_uppercase();
        assert!(upper.contains("PINEAPPLE") && upper.contains("BANANA"), "back on the host, claude lost a word: {reply}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Needs `TOD_TEST_CLAUDE_TOKEN` (a token made for tests) and a running
    /// container made from `tod-test:base` (`TOD_TEST_DEV_CONTAINER`).
    #[test]
    fn a_real_claude_resumes_a_session_moved_into_a_dev_container() {
        let (Ok(token), Ok(container)) = (std::env::var("TOD_TEST_CLAUDE_TOKEN"), std::env::var("TOD_TEST_DEV_CONTAINER")) else {
            eprintln!("skipped: set TOD_TEST_CLAUDE_TOKEN and TOD_TEST_DEV_CONTAINER (tod-test:base)");
            return;
        };
        let remote = ContainerRemote::new(&container).unwrap();
        let cwd = "/root/work";
        remote.exec.output("/", "mkdir", &["-p", cwd]).unwrap();
        let run = |args: &[String]| -> Result<std::process::Output> {
            let refs: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
            remote.exec.output(cwd, &args[0], &refs)
        };
        real_claude_move(&remote, cwd, &run, &token);
    }

    /// Needs `TOD_TEST_CLAUDE_TOKEN` and a sandbox made with `--agents`
    /// (`TOD_TEST_SANDBOX`, `TOD_TEST_SANDBOX_ROOT`).
    #[test]
    fn a_real_claude_resumes_a_session_moved_into_a_cloud_sandbox() {
        let (Ok(token), Ok(name), Ok(root)) = (
            std::env::var("TOD_TEST_CLAUDE_TOKEN"),
            std::env::var("TOD_TEST_SANDBOX"),
            std::env::var("TOD_TEST_SANDBOX_ROOT"),
        ) else {
            eprintln!("skipped: set TOD_TEST_CLAUDE_TOKEN, TOD_TEST_SANDBOX, TOD_TEST_SANDBOX_ROOT");
            return;
        };
        crate::fleet::sandbox::set_data_root(&std::fs::canonicalize(root).unwrap());
        let remote = SandboxRemote::new(&name);
        let cwd = "/root/work";
        remote.exec.output("/", "mkdir", &["-p", cwd]).unwrap();
        let run = |args: &[String]| -> Result<std::process::Output> {
            let refs: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
            remote.exec.output(cwd, &args[0], &refs)
        };
        real_claude_move(&remote, cwd, &run, &token);
    }
}
