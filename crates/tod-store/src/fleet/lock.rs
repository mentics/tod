use crate::fleet::paths::FleetPaths;
use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FleetLockError {
    #[error("another tod instance holds the fleet storage lock at {path}{}", held_by(holder))]
    InUse { path: String, holder: Option<String> },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Exclusive OS file lock on `tod.lock` held for the process lifetime.
pub struct FleetLock {
    _file: File,
    path: std::path::PathBuf,
}

impl FleetLock {
    /// Acquire the fleet lock, creating the storage root if needed.
    ///
    /// A lock file left behind by a crashed process is treated as stale when the
    /// exclusive lock can still be acquired (prior holder released the OS lock).
    pub fn try_acquire(root: impl AsRef<Path>) -> Result<Self, FleetLockError> {
        let paths = FleetPaths::new(root.as_ref())?;
        paths.ensure_root()?;

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(paths.lock())
            .with_context(|| format!("failed to open fleet lock {}", paths.lock().display()))?;

        file.try_lock_exclusive().map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                FleetLockError::InUse { path: paths.lock().display().to_string(), holder: read_holder(paths.lock()) }
            } else {
                FleetLockError::Other(e.into())
            }
        })?;
        // Say who holds it, so a process refused the lock can name it.
        // Best effort: the lock is what matters, and it is held.
        let mut file = file;
        let _ = write_holder(&mut file);

        Ok(Self {
            path: paths.lock().to_path_buf(),
            _file: file,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// What a holder writes into the lock file: its pid and command line.
fn write_holder(file: &mut File) -> std::io::Result<()> {
    use std::io::{Seek, Write};
    let command: Vec<String> = std::env::args().collect();
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    write!(file, "pid {}: {}", std::process::id(), command.join(" "))?;
    file.flush()
}

/// The holder's pid and command line, as it wrote them; `None` when they
/// cannot be read (Windows refuses reads of a locked file).
pub fn read_holder(lock: &Path) -> Option<String> {
    let text = std::fs::read_to_string(lock).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| cut(text, 300))
}

fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// " (held by …)" when the holder is known.
pub fn held_by(holder: &Option<String>) -> String {
    holder.as_deref().map(|h| format!(" (held by {h})")).unwrap_or_default()
}

impl Drop for FleetLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::thread;

    #[test]
    fn second_acquire_fails_while_held() {
        let root = std::env::temp_dir().join(format!("tod-fleet-lock-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        let lock1 = FleetLock::try_acquire(&root).unwrap();
        #[cfg(unix)]
        {
            let err = match FleetLock::try_acquire(&root) {
                Err(e) => e,
                Ok(_) => panic!("expected second acquire to fail"),
            };
            assert!(matches!(err, FleetLockError::InUse { .. }));
        }
        #[cfg(windows)]
        {
            // Windows may report same-process contention as OS error 33 instead of WouldBlock.
            let err = match FleetLock::try_acquire(&root) {
                Err(e) => e,
                Ok(second) => {
                    drop(second);
                    panic!("expected second acquire to fail on Windows");
                }
            };
            assert!(
                matches!(err, FleetLockError::InUse { .. }) || matches!(err, FleetLockError::Other(_))
            );
        }
        drop(lock1);

        // Stale lock file remains but OS lock was released — re-acquire succeeds.
        assert!(FleetPaths::new(&root).unwrap().lock().exists());
        let lock2 = FleetLock::try_acquire(&root).unwrap();
        drop(lock2);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn lock_survives_across_threads_in_same_process() {
        let root = std::env::temp_dir().join(format!("tod-fleet-lock-t-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let lock = FleetLock::try_acquire(&root).unwrap();
        let root2 = root.clone();
        let handle = thread::spawn(move || FleetLock::try_acquire(&root2));
        let err = match handle.join().unwrap() {
            Err(e) => e,
            Ok(_) => panic!("expected lock in use from other thread"),
        };
        assert!(matches!(err, FleetLockError::InUse { .. }));
        drop(lock);
        let _ = fs::remove_dir_all(root);
    }
}
