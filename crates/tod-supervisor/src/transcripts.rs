//! Mirroring Claude Code's session files as they are written, and restoring
//! them on a new sandbox so the agent's session can be resumed by id (design:
//! "Transcripts").
//!
//! Claude writes each session to `<claude dir>/projects/<project>/<id>.jsonl`
//! (`<claude dir>` is `$CLAUDE_CONFIG_DIR`, else `~/.claude`). Each file is
//! mirrored under the name `<project>__<id>`, appending complete lines only,
//! at the offset the copy is known to have, so a retried append is never
//! written twice. Where the copies go is a [`TranscriptStore`]: Agent Drive
//! ([`DirStore`], over the mount `tod_sandbox::drive` makes) where the
//! sandbox has it, else the orchestrator.

use crate::orchestrator::Orchestrator;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Where transcripts are mirrored.
pub trait TranscriptStore: Send + Sync + 'static {
    /// Every copy's name and size.
    fn list(&self) -> Result<Vec<(String, u64)>>;
    fn fetch(&self, name: &str) -> Result<Vec<u8>>;
    /// Appends `bytes` to `name` at `offset`: `Ok(new size)`, or `Err(size)`
    /// when the copy is not `offset` long.
    fn append(&self, name: &str, offset: u64, bytes: &[u8]) -> Result<std::result::Result<u64, u64>>;
}

impl TranscriptStore for Orchestrator {
    fn list(&self) -> Result<Vec<(String, u64)>> {
        self.list_transcripts()
    }
    fn fetch(&self, name: &str) -> Result<Vec<u8>> {
        self.fetch_transcript(name)
    }
    fn append(&self, name: &str, offset: u64, bytes: &[u8]) -> Result<std::result::Result<u64, u64>> {
        self.append_transcript(name, offset, bytes)
    }
}

/// Copies kept as `<dir>/<name>.jsonl` in a directory: the node's folder on
/// Agent Drive, mounted in the sandbox (`tod_sandbox::drive`). The mount is
/// an ordinary POSIX filesystem, so an append is an append.
pub struct DirStore {
    dir: PathBuf,
}

impl DirStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        anyhow::ensure!(split_name(name).is_some() && !name.contains(['/', '\\']), "bad transcript name {name:?}");
        Ok(self.dir.join(format!("{name}.jsonl")))
    }
}

impl TranscriptStore for DirStore {
    fn list(&self) -> Result<Vec<(String, u64)>> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Ok(Vec::new()) };
        Ok(entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().strip_suffix(".jsonl")?.to_string();
                Some((name, e.metadata().ok()?.len()))
            })
            .collect())
    }

    fn fetch(&self, name: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.path(name)?).unwrap_or_default())
    }

    fn append(&self, name: &str, offset: u64, bytes: &[u8]) -> Result<std::result::Result<u64, u64>> {
        use std::io::Write;
        let path = self.path(name)?;
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size != offset {
            return Ok(Err(size));
        }
        std::fs::create_dir_all(&self.dir)?;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(bytes)?;
        Ok(Ok(size + bytes.len() as u64))
    }
}

/// Where Agent Drive is mounted in a node's sandbox
/// (`tod_sandbox::drive::MOUNT_PATH`).
pub const DRIVE_MOUNT: &str = "/mnt/tod-transcripts";

/// The directory to mirror transcripts into instead of the orchestrator:
/// `$TOD_TRANSCRIPTS_DIR`, else [`DRIVE_MOUNT`] when something is mounted
/// there (`/proc/mounts`). A sandbox whose drive could not be mounted has
/// neither, and its transcripts go to the orchestrator.
pub fn drive_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TOD_TRANSCRIPTS_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
    mounted_at(&mounts, DRIVE_MOUNT).then(|| PathBuf::from(DRIVE_MOUNT))
}

fn mounted_at(proc_mounts: &str, path: &str) -> bool {
    proc_mounts.lines().any(|l| l.split_whitespace().nth(1) == Some(path))
}

/// `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`.
pub fn default_projects_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("projects"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".claude").join("projects"))
}

const SEPARATOR: &str = "__";

fn mirror_name(project: &str, session: &str) -> Option<String> {
    let name = format!("{project}{SEPARATOR}{session}");
    let ok = !name.starts_with('.')
        && name.len() <= 200
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    ok.then_some(name)
}

fn split_name(name: &str) -> Option<(&str, &str)> {
    let (project, session) = name.split_once(SEPARATOR)?;
    (!project.is_empty() && !session.is_empty() && !project.contains("..") && !session.contains(".."))
        .then_some((project, session))
}

/// Follows the session files under a projects directory.
pub struct Mirror {
    projects: PathBuf,
    store: Arc<dyn TranscriptStore>,
    /// Mirror name -> bytes the copy is known to hold.
    offsets: Mutex<HashMap<String, u64>>,
}

impl Mirror {
    pub fn new(projects: PathBuf, store: Arc<dyn TranscriptStore>) -> Self {
        Self { projects, store, offsets: Mutex::new(HashMap::new()) }
    }

    /// Copies back every mirrored session missing here or shorter here than
    /// its copy. Run before the agent starts, on a new sandbox. The number
    /// restored.
    pub fn restore(&self) -> Result<usize> {
        let mut restored = 0;
        let mut offsets = self.offsets.lock().unwrap_or_else(|e| e.into_inner());
        for (name, size) in self.store.list()? {
            let Some((project, session)) = split_name(&name) else { continue };
            let path = self.projects.join(project).join(format!("{session}.jsonl"));
            let local = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if local < size {
                let bytes = self.store.fetch(&name)?;
                std::fs::create_dir_all(path.parent().expect("has a parent"))?;
                std::fs::write(&path, &bytes)?;
                restored += 1;
                offsets.insert(name, bytes.len() as u64);
            } else {
                offsets.insert(name, size);
            }
        }
        Ok(restored)
    }

    /// Sends every complete line written since the last pass. The bytes sent.
    pub fn sync_once(&self) -> Result<u64> {
        let mut sent = 0;
        let Ok(projects) = std::fs::read_dir(&self.projects) else { return Ok(0) };
        let mut offsets = self.offsets.lock().unwrap_or_else(|e| e.into_inner());
        for project in projects.flatten() {
            let project_name = project.file_name().to_string_lossy().into_owned();
            let Ok(files) = std::fs::read_dir(project.path()) else { continue };
            for file in files.flatten() {
                let file_name = file.file_name().to_string_lossy().into_owned();
                let Some(session) = file_name.strip_suffix(".jsonl") else { continue };
                let Some(name) = mirror_name(&project_name, session) else { continue };
                sent += sync_file(&*self.store, &file.path(), &name, &mut offsets)?;
            }
        }
        Ok(sent)
    }

    /// Mirrors every `every` on a thread until the handle is stopped, with
    /// one last pass then.
    pub fn follow(self: Arc<Self>, every: Duration) -> Follow {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("tod-supervisor-transcripts".into())
            .spawn(move || {
                loop {
                    let stopping = flag.load(Ordering::SeqCst);
                    if let Err(err) = self.sync_once() {
                        tracing::warn!("mirroring transcripts: {err:#}");
                    }
                    if stopping {
                        break;
                    }
                    let mut slept = Duration::ZERO;
                    while slept < every && !flag.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(50));
                        slept += Duration::from_millis(50);
                    }
                }
            })
            .ok();
        Follow { stop, thread }
    }
}

fn sync_file(store: &dyn TranscriptStore, path: &Path, name: &str, offsets: &mut HashMap<String, u64>) -> Result<u64> {
    let bytes = std::fs::read(path)?;
    // Complete lines only: the agent may be mid-write.
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1) as u64;
    let mut offset = offsets.get(name).copied().unwrap_or(0);
    let mut sent = 0;
    for _ in 0..3 {
        if complete <= offset {
            break;
        }
        match store.append(name, offset, &bytes[offset as usize..complete as usize])? {
            Ok(size) => {
                sent += size - offset;
                offset = size;
            }
            // The copy has more or less than we thought (a restart): go on
            // from what it has, unless it has more than the file.
            Err(size) if size <= complete => offset = size,
            Err(size) => {
                tracing::warn!(%name, size, local = complete, "the transcript copy is longer than the file; leaving it");
                offset = size;
                break;
            }
        }
    }
    offsets.insert(name.to_string(), offset);
    Ok(sent)
}

/// A running [`Mirror::follow`]; stopping it makes one last pass.
pub struct Follow {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Follow {
    pub fn stop(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Memory(Mutex<HashMap<String, Vec<u8>>>);

    impl TranscriptStore for Memory {
        fn list(&self) -> Result<Vec<(String, u64)>> {
            Ok(self.0.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.len() as u64)).collect())
        }
        fn fetch(&self, name: &str) -> Result<Vec<u8>> {
            Ok(self.0.lock().unwrap().get(name).cloned().unwrap_or_default())
        }
        fn append(&self, name: &str, offset: u64, bytes: &[u8]) -> Result<std::result::Result<u64, u64>> {
            let mut map = self.0.lock().unwrap();
            let copy = map.entry(name.to_string()).or_default();
            if copy.len() as u64 != offset {
                return Ok(Err(copy.len() as u64));
            }
            copy.extend_from_slice(bytes);
            Ok(Ok(copy.len() as u64))
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-sup-tr-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mirrors_complete_lines_and_restores_them() {
        let projects = temp("mirror");
        let file = projects.join("-workspace-repo").join("abc-123.jsonl");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{\"a\":1}\n{\"b\":").unwrap();
        let store = Arc::new(Memory::default());
        let mirror = Mirror::new(projects.clone(), store.clone());
        assert_eq!(mirror.sync_once().unwrap(), 8);
        std::fs::write(&file, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        assert_eq!(mirror.sync_once().unwrap(), 8);
        assert_eq!(mirror.sync_once().unwrap(), 0);
        assert_eq!(store.fetch("-workspace-repo__abc-123").unwrap(), b"{\"a\":1}\n{\"b\":2}\n");

        // A new sandbox: nothing on disk, restored from the copy.
        let fresh = temp("restore");
        let mirror = Mirror::new(fresh.clone(), store.clone());
        assert_eq!(mirror.restore().unwrap(), 1);
        assert_eq!(
            std::fs::read(fresh.join("-workspace-repo").join("abc-123.jsonl")).unwrap(),
            b"{\"a\":1}\n{\"b\":2}\n"
        );
        // Restored bytes are not sent again; new ones are.
        assert_eq!(mirror.sync_once().unwrap(), 0);
        let _ = std::fs::remove_dir_all(projects);
        let _ = std::fs::remove_dir_all(fresh);
    }

    #[test]
    fn a_directory_store_appends_at_the_offset_it_has() {
        let dir = temp("dir").join("nested");
        let store = DirStore::new(dir.clone());
        assert_eq!(store.append("p__s", 0, b"1\n").unwrap(), Ok(2));
        assert_eq!(store.append("p__s", 2, b"2\n").unwrap(), Ok(4));
        // A retry of the first append is refused, and says how long the copy is.
        assert_eq!(store.append("p__s", 0, b"1\n").unwrap(), Err(4));
        assert_eq!(store.fetch("p__s").unwrap(), b"1\n2\n");
        assert_eq!(store.list().unwrap(), vec![("p__s".to_string(), 4)]);
        assert!(store.append("../escape__x", 0, b"x").is_err());
        assert!(store.append("noseparator", 0, b"x").is_err());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn the_drive_is_used_when_it_is_mounted() {
        let mounts = "overlay / overlay rw 0 0\n1.2.3.4:/b/users/u/nodes/n/transcripts /mnt/tod-transcripts fuse.blfs rw 0 0\n";
        assert!(mounted_at(mounts, DRIVE_MOUNT));
        assert!(!mounted_at("overlay / overlay rw 0 0\n", DRIVE_MOUNT));
    }

    #[test]
    fn a_forgotten_offset_resumes_from_the_copy() {
        let projects = temp("offset");
        let file = projects.join("p").join("s.jsonl");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "1\n2\n").unwrap();
        let store = Arc::new(Memory::default());
        store.append("p__s", 0, b"1\n").unwrap().unwrap();
        let mirror = Mirror::new(projects.clone(), store.clone());
        mirror.sync_once().unwrap();
        assert_eq!(store.fetch("p__s").unwrap(), b"1\n2\n");
        let _ = std::fs::remove_dir_all(projects);
    }
}
