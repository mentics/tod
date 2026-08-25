use crate::interview::queue::{QueueQuestion, load_queue_dir};
use anyhow::{Context, Result};
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(300);

pub struct QueueWatcher {
    queue_dir: PathBuf,
    watcher: RecommendedWatcher,
    receiver: Receiver<Result<Event, notify::Error>>,
    debounce_deadline: Option<Instant>,
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
        Ok(Self {
            queue_dir,
            watcher,
            receiver: rx,
            debounce_deadline: None,
        })
    }

    pub fn queue_dir(&self) -> &Path {
        &self.queue_dir
    }

    /// Poll filesystem events without blocking the UI thread.
    /// Returns `Some(updated questions)` after debounce settles.
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

        if self
            .debounce_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.debounce_deadline = None;
            return Ok(Some(load_queue_dir(&self.queue_dir)?));
        }
        Ok(None)
    }

    pub fn snapshot(&self) -> Result<Vec<QueueQuestion>> {
        load_queue_dir(&self.queue_dir)
    }
}

fn is_queue_relevant(event: &Event) -> bool {
    match &event.kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Data(_)) => true,
        EventKind::Modify(ModifyKind::Name(mode)) => {
            matches!(mode, RenameMode::Any | RenameMode::To | RenameMode::Both)
        }
        _ => false,
    }
}

/// Per-session watcher registry keyed by scratchpad queue path.
pub struct QueueWatcherRegistry {
    watchers: HashMap<PathBuf, QueueWatcher>,
}

impl QueueWatcherRegistry {
    pub fn new() -> Self {
        Self {
            watchers: HashMap::new(),
        }
    }

    pub fn ensure(&mut self, queue_dir: PathBuf) -> Result<()> {
        if !self.watchers.contains_key(&queue_dir) {
            let watcher = QueueWatcher::new(queue_dir.clone())?;
            self.watchers.insert(queue_dir, watcher);
        }
        Ok(())
    }

    pub fn poll_all(&mut self) -> Result<HashMap<PathBuf, Vec<QueueQuestion>>> {
        let keys: Vec<PathBuf> = self.watchers.keys().cloned().collect();
        let mut updates = HashMap::new();
        for key in keys {
            if let Some(watcher) = self.watchers.get_mut(&key) {
                if let Some(questions) = watcher.poll()? {
                    updates.insert(key.clone(), questions);
                }
            }
        }
        Ok(updates)
    }

    pub fn remove(&mut self, queue_dir: &Path) {
        self.watchers.remove(queue_dir);
    }
}

impl Default for QueueWatcherRegistry {
    fn default() -> Self {
        Self::new()
    }
}
