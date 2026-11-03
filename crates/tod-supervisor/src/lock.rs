//! One supervisor per node at a time: an exclusive `flock` on
//! `<state_dir>/supervisor.lock`, held for the whole wake.
//!
//! Two can otherwise run at once (the relay's poke, a schedule's fallback
//! that runs the supervisor itself, the dead-man retry wake of
//! `waits::arm_retry` firing while a long run goes on), each driving the
//! autopilot and the replica. The loser exits at once, saying so
//! ([`crate::Woke::AlreadyRunning`]), without touching the schedule, the
//! replica, or the heartbeat, and pokes the holder (`SIGUSR1`) so it looks
//! again. The lock is the kernel's: it goes with the process, however it ends.
//!
//! The holder's pid is written into the file, for the poke and for the relay,
//! which reads it to find a supervisor it did not start (`tod-relay`'s
//! `liveness`). The file name is part of that contract.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// The lock file's name in the state directory (`tod-relay` reads it too).
pub const LOCK_FILE: &str = "supervisor.lock";

/// The lock, held until dropped.
#[derive(Debug)]
pub struct SupervisorLock {
    _file: File,
}

/// What taking the lock came to.
#[derive(Debug)]
pub enum Taken {
    Held(SupervisorLock),
    /// Another supervisor holds it; its pid when it had written it yet.
    Busy { holder: Option<u32> },
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(LOCK_FILE)
}

/// Takes the lock without waiting. An error is a lock that could not be
/// asked for (the directory, the file, `flock` itself), never "busy".
pub fn acquire(state_dir: &Path) -> Result<Taken> {
    std::fs::create_dir_all(state_dir).with_context(|| format!("create {}", state_dir.display()))?;
    let lock_path = path(state_dir);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: flock on a descriptor this function owns.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                let mut text = String::new();
                file.read_to_string(&mut text).with_context(|| format!("read {}", lock_path.display()))?;
                return Ok(Taken::Busy { holder: text.trim().parse().ok() });
            }
            return Err(err).with_context(|| format!("lock {}", lock_path.display()));
        }
    }
    file.set_len(0).with_context(|| format!("write {}", lock_path.display()))?;
    file.rewind().with_context(|| format!("write {}", lock_path.display()))?;
    write!(file, "{}", std::process::id()).with_context(|| format!("write {}", lock_path.display()))?;
    file.flush().with_context(|| format!("write {}", lock_path.display()))?;
    Ok(Taken::Held(SupervisorLock { _file: file }))
}

/// Tells the holder to look again (`SIGUSR1`, as the relay's poke does).
pub fn poke_holder(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: a signal to the pid the lock file names.
        if unsafe { libc::kill(pid as i32, libc::SIGUSR1) } != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| format!("signal supervisor pid {pid}"));
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_second_acquire_is_busy_and_names_the_holder() {
        let dir = tempfile_dir("lock-busy");
        let first = match acquire(&dir).unwrap() {
            Taken::Held(l) => l,
            Taken::Busy { .. } => panic!("the first must take it"),
        };
        match acquire(&dir).unwrap() {
            Taken::Busy { holder } => assert_eq!(holder, Some(std::process::id())),
            Taken::Held(_) => panic!("two holders"),
        }
        drop(first);
        assert!(matches!(acquire(&dir).unwrap(), Taken::Held(_)), "released with the first");
    }

    pub(crate) fn tempfile_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("tod-supervisor-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
