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
const CHUNK: usize = 48 * 1024;

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

impl Remote for SandboxRemote {
    fn list(&self) -> Result<Vec<RemoteLog>> {
        let out = self.sh(&list_script(), &[])?;
        Ok(parse_listing(&String::from_utf8_lossy(&out)))
    }

    fn read_from(&self, project: &str, session: &str, offset: u64) -> Result<Vec<u8>> {
        let script = format!(r#"{PROJECTS}; tail -c +"$(( $1 + 1 ))" "$d/$2/$3.jsonl""#);
        self.sh(&script, &[&offset.to_string(), project, session])
    }

    fn write(&self, project: &str, session: &str, bytes: &[u8]) -> Result<()> {
        // Into a file of its own first, so a cut-off copy is never a log.
        for (n, chunk) in bytes.chunks(CHUNK).enumerate() {
            let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
            let redirect = if n == 0 { ">" } else { ">>" };
            let script = format!(
                r#"{PROJECTS}; mkdir -p "$d/$1" && printf %s "$3" | base64 -d {redirect} "$d/$1/$2.jsonl.restoring""#
            );
            self.sh(&script, &[project, session, &encoded])?;
        }
        let script = format!(
            r#"{PROJECTS}; if [ -f "$d/$1/$2.jsonl.restoring" ]; then mv "$d/$1/$2.jsonl.restoring" "$d/$1/$2.jsonl"; else : > "$d/$1/$2.jsonl"; fi"#
        );
        self.sh(&script, &[project, session])?;
        Ok(())
    }
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
