use crate::interview::queue::{QueueQuestion, load_queue_dir};
use anyhow::{Context, Result};
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(300);

pub struct QueueWatcher {
    queue_dir: PathBuf,
    _watcher: RecommendedWatcher,
    receiver: Receiver<Result<Event, notify::Error>>,
    debounce_deadline: Option<Instant>,
    /// Fingerprint of queue dir contents — catches Windows notify gaps.
    last_fingerprint: Option<u64>,
}

impl QueueWatcher {
    pub fn new(queue_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&queue_dir).with_context(|| {
            format!(
                "failed to create queue dir for watcher {}",
                queue_dir.display()
            )
        })?;
        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            Config::default(),
        )?;
        watcher.watch(&queue_dir, RecursiveMode::NonRecursive)?;
        let last_fingerprint = Some(dir_fingerprint(&queue_dir));
        Ok(Self {
            queue_dir,
            _watcher: watcher,
            receiver: rx,
            debounce_deadline: None,
            last_fingerprint,
        })
    }

    /// Poll filesystem events without blocking the UI thread.
    /// Returns `Some(updated questions)` after debounce settles, or when the
    /// on-disk fingerprint changes (notify miss / delayed events).
    pub fn poll(&mut self) -> Result<Option<Vec<QueueQuestion>>> {
        while let Ok(result) = self.receiver.try_recv() {
            match result {
                Ok(event) if is_queue_relevant(&event) => {
                    self.debounce_deadline = Some(Instant::now() + DEBOUNCE);
                }
                Ok(_) => {}
                Err(err) => return Err(err.into()),
            }
        }

        let fp = dir_fingerprint(&self.queue_dir);
        let fingerprint_dirty = self.last_fingerprint.is_some_and(|prev| prev != fp);
        let debounce_ready = self
            .debounce_deadline
            .is_some_and(|deadline| Instant::now() >= deadline);

        if debounce_ready {
            self.debounce_deadline = None;
        }

        if debounce_ready || fingerprint_dirty {
            let questions = load_queue_dir(&self.queue_dir)?;
            self.last_fingerprint = Some(dir_fingerprint(&self.queue_dir));
            return Ok(Some(questions));
        }
        Ok(None)
    }
}

/// Stable fingerprint of `*.md` queue files (name + len + mtime).
fn dir_fingerprint(queue_dir: &Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut entries: Vec<(String, u64, u64)> = Vec::new();
    if let Ok(read) = std::fs::read_dir(queue_dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let meta = entry.metadata().ok();
            let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            entries.push((name, len, mtime));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = DefaultHasher::new();
    entries.hash(&mut hasher);
    hasher.finish()
}

fn is_queue_relevant(event: &Event) -> bool {
    match &event.kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Data(_)) => true,
        EventKind::Modify(ModifyKind::Name(mode)) => {
            matches!(mode, RenameMode::Any | RenameMode::To | RenameMode::Both)
        }
        // Windows often reports opaque Modify(Any) for new files.
        EventKind::Modify(ModifyKind::Any) => true,
        EventKind::Any => true,
        _ => false,
    }
}
