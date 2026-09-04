use crate::fleet::paths::FleetPaths;
use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FleetLockError {
    #[error("another tod instance holds the fleet storage lock at {0}")]
    InUse(String),
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
                FleetLockError::InUse(paths.lock().display().to_string())
            } else {
                FleetLockError::Other(e.into())
            }
        })?;

        Ok(Self {
            path: paths.lock().to_path_buf(),
            _file: file,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
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
            assert!(matches!(err, FleetLockError::InUse(_)));
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
                matches!(err, FleetLockError::InUse(_)) || matches!(err, FleetLockError::Other(_))
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
        assert!(matches!(err, FleetLockError::InUse(_)));
        drop(lock);
        let _ = fs::remove_dir_all(root);
    }
}
