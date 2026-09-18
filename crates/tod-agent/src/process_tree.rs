//! Agent processes that do not outlive tod.
//!
//! An agent is a process tree, not a process: on Windows `cmd` → `node` →
//! whatever the agent runs (a build, a test run), elsewhere the adapter and
//! its subprocesses. Killing the direct child leaves the rest, and when tod
//! dies without cleaning up — a crash, a force kill — nothing stops any of it.
//! So every agent is spawned into a container the OS tears down with tod:
//!
//! - **Windows**: a job object per agent, with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. tod holds its only handle; however
//!   tod exits, the handle closes and Windows kills every process in the job.
//!   The child is created suspended and resumed only once it is in the job, so
//!   nothing it starts can escape before then.
//! - **macOS and Linux**: the agent leads its own process group, and a
//!   watchdog `sh` blocks reading a pipe whose only writer is tod. When tod
//!   exits the read hits EOF and the watchdog kills the group. The watchdog
//!   leads a group of its own, so a Ctrl+C sent to tod's terminal does not
//!   take it down before it has done its job.
//!
//! If the container cannot be set up, the agent still runs, without it, and
//! a warning is logged.
//!
//! [`AgentProcess`] kills its tree when dropped, so it behaves the same
//! whether tod lets go of it deliberately or dies holding it.

use std::io;
use std::ops::{Deref, DerefMut};
use std::process::{Child, Command};

/// A spawned agent and the container its tree lives in. Derefs to the
/// [`Child`] for its pipes and pid.
pub struct AgentProcess {
    child: Child,
    tree: Option<imp::Tree>,
    killed: bool,
}

impl AgentProcess {
    /// Kill the agent and everything it started, and reap it. Idempotent.
    pub fn kill_tree(&mut self) {
        if std::mem::replace(&mut self.killed, true) {
            return;
        }
        imp::kill(&mut self.child, self.tree.as_mut());
        let _ = self.child.wait();
    }

    /// Whether the agent is in a container that dies with tod.
    #[allow(dead_code)]
    pub fn contained(&self) -> bool {
        self.tree.is_some()
    }
}

impl Deref for AgentProcess {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for AgentProcess {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        self.kill_tree();
    }
}

/// Spawn `command` as an agent whose whole tree dies with tod.
///
/// On Windows this sets `command`'s creation flags; on Unix, its process
/// group. Callers must not set either.
pub fn spawn(command: &mut Command) -> io::Result<AgentProcess> {
    imp::prepare(command);
    let child = command.spawn()?;
    let (child, tree) = imp::contain(child)?;
    Ok(AgentProcess {
        child,
        tree,
        killed: false,
    })
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };

    /// A kill-on-close job holding the agent's tree. Closing the handle —
    /// on drop, or by Windows when tod exits — kills everything in it.
    pub struct Tree {
        job: HANDLE,
    }

    // A job handle is a kernel handle, usable from any thread.
    unsafe impl Send for Tree {}

    impl Drop for Tree {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.job) };
        }
    }

    pub fn prepare(command: &mut Command) {
        command.creation_flags(CREATE_SUSPENDED);
    }

    pub fn contain(mut child: Child) -> io::Result<(Child, Option<Tree>)> {
        let tree = match job_for(&child) {
            Ok(tree) => Some(tree),
            Err(err) => {
                tracing::warn!(
                    pid = child.id(),
                    "agent runs outside a job object and may outlive tod: {err}"
                );
                None
            }
        };
        if let Err(err) = resume(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
        Ok((child, tree))
    }

    fn job_for(child: &Child) -> io::Result<Tree> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let tree = Tree { job };
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(tree)
        }
    }

    /// Resume the one thread of a process created suspended. `Child` does not
    /// keep the thread handle `CreateProcess` returned, so find it.
    fn resume(pid: u32) -> io::Result<()> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut resumed = false;
            let mut more = Thread32First(snapshot, &mut entry) != 0;
            while more {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        resumed |= ResumeThread(thread) != u32::MAX;
                        CloseHandle(thread);
                    }
                }
                more = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            if resumed {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "could not resume agent process {pid}"
                )))
            }
        }
    }

    pub fn kill(child: &mut Child, tree: Option<&mut Tree>) {
        match tree {
            Some(tree) => unsafe {
                TerminateJobObject(tree.job, 1);
            },
            // No job: the best that can be done is the tree as it stands now.
            None => {
                let _ = Command::new("taskkill")
                    .args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW)
                    .status();
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, ChildStdin, Command, Stdio};

    /// The agent's process group, and the watchdog that kills it if tod
    /// exits without doing so.
    pub struct Tree {
        pgid: libc::pid_t,
        watchdog: Option<(Child, ChildStdin)>,
    }

    pub fn prepare(command: &mut Command) {
        command.process_group(0);
    }

    pub fn contain(child: Child) -> io::Result<(Child, Option<Tree>)> {
        let pgid = child.id() as libc::pid_t;
        let watchdog = match watchdog(pgid) {
            Ok(watchdog) => Some(watchdog),
            Err(err) => {
                tracing::warn!(
                    pid = pgid,
                    "agent has no watchdog and may outlive tod: {err}"
                );
                None
            }
        };
        Ok((child, Some(Tree { pgid, watchdog })))
    }

    /// `sh` waiting for EOF on a pipe only tod writes to. The write end is
    /// close-on-exec, so no other child of tod holds it open.
    fn watchdog(pgid: libc::pid_t) -> io::Result<(Child, ChildStdin)> {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"read x; kill -KILL -"$1" 2>/dev/null"#)
            .arg("tod-agent-watchdog")
            .arg(pgid.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        Ok((child, stdin))
    }

    pub fn kill(child: &mut Child, tree: Option<&mut Tree>) {
        let Some(tree) = tree else {
            let _ = child.kill();
            return;
        };
        // Stop the watchdog first: once the group is killed and reaped its id
        // is free, and a watchdog still waiting could kill whatever gets it.
        if let Some((mut watchdog, stdin)) = tree.watchdog.take() {
            let _ = watchdog.kill();
            let _ = watchdog.wait();
            drop(stdin);
        }
        unsafe { libc::killpg(tree.pgid, libc::SIGKILL) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const OWNER_ENV: &str = "TOD_PROCESS_TREE_TEST_OWNER";

    /// An agent whose direct child is a wrapper around a long-running
    /// grandchild, like `cmd` → `node`. Prints the grandchild's pid first.
    fn agent_command() -> Command {
        #[cfg(windows)]
        {
            let mut command = Command::new("powershell");
            command.args([
                "-NoProfile",
                "-Command",
                "$p = Start-Process -PassThru -WindowStyle Hidden ping '-n 120 127.0.0.1'; \
                 Write-Output $p.Id; [Console]::Out.Flush(); Start-Sleep 120",
            ]);
            command
        }
        #[cfg(unix)]
        {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "sleep 120 & echo $!; wait"]);
            command
        }
    }

    /// Spawn the test agent and return it with its grandchild's pid.
    fn spawn_agent() -> (AgentProcess, u32) {
        let mut agent = spawn(
            agent_command()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .expect("spawn agent");
        let mut line = String::new();
        BufReader::new(agent.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let grandchild = line.trim().parse().expect("grandchild pid");
        (agent, grandchild)
    }

    fn alive(pid: u32) -> bool {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code) != 0;
            CloseHandle(handle);
            ok && code == STILL_ACTIVE as u32
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, 0) == 0
        }
    }

    fn gone_within(pid: u32, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn kill_tree_takes_the_grandchild() {
        let (mut agent, grandchild) = spawn_agent();
        assert!(agent.contained());
        assert!(alive(grandchild));
        let pid = agent.id();
        agent.kill_tree();
        assert!(gone_within(pid, Duration::from_secs(5)), "agent {pid}");
        assert!(
            gone_within(grandchild, Duration::from_secs(5)),
            "grandchild {grandchild}"
        );
    }

    /// The case this module exists for: the process holding the agent dies
    /// without cleaning up. The owner is this test binary run again as
    /// [`owner`], killed outright the way a crash or Task Manager would.
    #[test]
    fn the_tree_dies_with_its_owner() {
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_tree::tests::owner",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(OWNER_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn owner");
        let pids: Vec<u32> = BufReader::new(owner.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok)
            // libtest may print the test's name ahead of it on the same line.
            .find_map(|line| {
                let (_, rest) = line.split_once("agent pids: ")?;
                Some(rest.split(' ').map(|p| p.parse().unwrap()).collect())
            })
            .expect("owner reports its agent");
        for &pid in &pids {
            assert!(alive(pid), "{pid} before the owner dies");
        }

        owner.kill().unwrap();
        owner.wait().unwrap();

        for pid in pids {
            assert!(
                gone_within(pid, Duration::from_secs(5)),
                "{pid} outlived its owner"
            );
        }
    }

    /// Run only by [`the_tree_dies_with_its_owner`]: hold an agent until
    /// killed.
    #[test]
    #[ignore]
    fn owner() {
        if std::env::var_os(OWNER_ENV).is_none() {
            return;
        }
        let (agent, grandchild) = spawn_agent();
        println!("agent pids: {} {grandchild}", agent.id());
        std::thread::sleep(Duration::from_secs(120));
        drop(agent);
    }
}
