//! Reaping orphaned processes.
//!
//! A sandbox's init does not reap orphans, so every process whose parent
//! exits before it does stays a `<defunct>` zombie once it ends: the `claude`
//! processes an agent adapter starts, killed with its process group when the
//! supervisor ends a session, are the common case. The relay is the ancestor
//! of everything tod starts in the sandbox and lives as long as the sandbox,
//! so it makes itself a child subreaper (`PR_SET_CHILD_SUBREAPER`): orphans
//! below it are reparented to it instead of to init, and it reaps them.
//!
//! It must not reap its own children, whose exit statuses the code that
//! spawned them waits for (`std::process::Child`, `tokio::process::Child`).
//! So every child the relay spawns is spawned through [`spawn_tracked`],
//! which records its pid, and [`reap_orphans`] reaps only zombies whose
//! parent is the relay and whose pid is not recorded. A blanket
//! `waitpid(-1)` would take whichever child ended first, tracked or not.
//! Spawning and reaping hold the same lock, so a child that ends before its
//! pid is recorded is never mistaken for an orphan; the owner calls
//! [`untrack`] once it has waited for it.
//!
//! This lives in the relay rather than the supervisor: the supervisor is
//! short-lived (one wake), so orphans of the sandbox's other processes
//! (terminals, `/exec` commands) and anything still running when it exits
//! would be left over anyway, and its many library waits (git, the agent
//! transport) are not all under its own control to track.

use std::collections::HashSet;
use std::io;
use std::sync::Mutex;
use std::time::Duration;

/// How often orphans are looked for. A zombie holds only a process-table
/// slot, so a few seconds' delay costs nothing.
const EVERY: Duration = Duration::from_secs(5);

/// The relay's own children not yet waited for.
static TRACKED: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

fn tracked() -> std::sync::MutexGuard<'static, Option<HashSet<u32>>> {
    TRACKED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Makes this process a child subreaper and reaps orphans every [`EVERY`]
/// from a background thread. Failing that, it logs and carries on: the
/// relay works without it, only zombies pile up.
pub fn start() {
    if let Err(err) = become_subreaper() {
        eprintln!("reaper: could not become a child subreaper: {err}");
        return;
    }
    let spawned = std::thread::Builder::new().name("reaper".into()).spawn(|| loop {
        std::thread::sleep(EVERY);
        for pid in reap_orphans() {
            eprintln!("reaper: reaped orphan pid {pid}");
        }
    });
    if let Err(err) = spawned {
        eprintln!("reaper: could not start: {err}");
    }
}

fn become_subreaper() -> io::Result<()> {
    // SAFETY: prctl with an integer argument; touches no memory.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Spawns a child of the relay with `spawn` and records its pid (`pid_of`),
/// so [`reap_orphans`] leaves its exit status for the owner's wait.
pub fn spawn_tracked<T>(spawn: impl FnOnce() -> io::Result<T>, pid_of: impl FnOnce(&T) -> Option<u32>) -> io::Result<T> {
    let mut set = tracked();
    let spawned = spawn()?;
    if let Some(pid) = pid_of(&spawned) {
        set.get_or_insert_with(HashSet::new).insert(pid);
    }
    Ok(spawned)
}

/// Forgets a tracked child once its owner has waited for it.
pub fn untrack(pid: u32) {
    if let Some(set) = tracked().as_mut() {
        set.remove(&pid);
    }
}

/// Reaps every zombie whose parent is this process and that it did not
/// spawn itself (see the module docs). Returns the pids reaped.
pub fn reap_orphans() -> Vec<u32> {
    let set = tracked();
    let me = std::process::id();
    let mut reaped = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else { return reaped };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        if set.as_ref().is_some_and(|s| s.contains(&pid)) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue };
        if parse_stat(&stat) != Some(('Z', me)) {
            continue;
        }
        // A zombie child's pid cannot be reused before its parent (this
        // process) reaps it, so this is the process just read.
        let mut status = 0;
        // SAFETY: waitpid on one pid, writing to a local.
        if unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) } == pid as libc::pid_t {
            reaped.push(pid);
        }
    }
    reaped
}

/// A process's state and parent pid from its `/proc/<pid>/stat`. The name
/// (in parentheses) may hold spaces and parentheses, so fields are read after
/// the last `)`.
fn parse_stat(stat: &str) -> Option<(char, u32)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((state, ppid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    #[test]
    fn stat_fields_come_after_the_name() {
        assert_eq!(parse_stat("42 (a) b (c)) Z 7 42 42 0 -1"), Some(('Z', 7)));
        assert_eq!(parse_stat("42 (sh) S 1 42"), Some(('S', 1)));
        assert_eq!(parse_stat("garbage"), None);
    }

    fn state(pid: u32) -> Option<(char, u32)> {
        parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// An orphan reparented to the relay is reaped; a tracked child that has
    /// exited but not yet been waited for keeps its exit status for its owner.
    #[test]
    fn reaps_orphans_but_not_tracked_children() {
        become_subreaper().unwrap();
        let me = std::process::id();

        // A tracked child that exits 7 and is not waited for yet.
        let mut own = spawn_tracked(|| Command::new("/bin/sh").args(["-c", "exit 7"]).spawn(), |c| Some(c.id())).unwrap();
        // A shell that leaves a background job behind and exits: the job is
        // reparented to this process (the subreaper) and ends a moment later.
        let mut parent = spawn_tracked(
            || Command::new("/bin/sh").args(["-c", "sleep 0.3 >/dev/null & echo $!"]).stdout(Stdio::piped()).spawn(),
            |c| Some(c.id()),
        )
        .unwrap();
        let mut out = String::new();
        parent.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        parent.wait().unwrap();
        untrack(parent.id());
        let orphan: u32 = out.trim().parse().unwrap();

        until("the orphan to be a zombie of this process", || state(orphan) == Some(('Z', me)));
        until("the tracked child to exit", || state(own.id()).is_some_and(|(s, _)| s == 'Z'));

        let reaped = reap_orphans();
        assert!(reaped.contains(&orphan), "{reaped:?}");
        assert!(!reaped.contains(&own.id()), "{reaped:?}");
        assert_eq!(state(orphan), None, "the orphan is gone");

        // The owner still gets its child's status.
        assert_eq!(own.wait().unwrap().code(), Some(7));
        untrack(own.id());
    }
}
