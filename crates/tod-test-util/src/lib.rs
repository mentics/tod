//! Scratch directories for tests that leave nothing behind.
//!
//! A test that makes `std::env::temp_dir().join(..)` and returns a bare path
//! leaks the directory whenever it fails (or never cleans at all); a full run
//! once left 82,000 directories (110 GB) and filled the disk. Take the path from
//! [`scratch_dir`] instead: the directory is removed when the calling test's
//! thread ends (libtest runs each test on its own thread), even when the test
//! panics. `scripts/check-test-leaks.sh` fails the build when anything is left.

use std::cell::RefCell;
use std::path::PathBuf;

thread_local! {
    static SCRATCH: RefCell<Vec<tempfile::TempDir>> = const { RefCell::new(Vec::new()) };
}

/// A fresh, existing directory under the system temp dir, named `<prefix><random>`,
/// removed (with everything in it) when the calling thread ends.
///
/// Anything that must outlive the test's thread (a server thread still writing)
/// should own a `tempfile::TempDir` itself instead.
pub fn scratch_dir(prefix: impl AsRef<str>) -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix(prefix.as_ref())
        .tempdir()
        .expect("make a scratch directory");
    let path = dir.path().to_path_buf();
    SCRATCH.with(|s| s.borrow_mut().push(dir));
    path
}

#[cfg(test)]
mod tests {
    use super::scratch_dir;

    #[test]
    fn a_scratch_dir_is_removed_when_the_test_thread_ends() {
        let path = std::thread::spawn(|| {
            let p = scratch_dir("tod-scratch-check-");
            assert!(p.is_dir());
            std::fs::write(p.join("f"), b"x").unwrap();
            p
        })
        .join()
        .unwrap();
        assert!(!path.exists(), "{} was left behind", path.display());
    }

    #[test]
    fn a_scratch_dir_is_removed_when_the_test_panics() {
        let (tx, rx) = std::sync::mpsc::channel();
        let result = std::thread::spawn(move || {
            let p = scratch_dir("tod-scratch-panic-");
            tx.send(p).unwrap();
            panic!("fails on purpose");
        })
        .join();
        assert!(result.is_err());
        let path = rx.recv().unwrap();
        assert!(!path.exists(), "{} was left behind", path.display());
    }
}
