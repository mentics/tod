//! Process birth-time reconnect identity for agent and shell rows.

use sysinfo::{Pid, ProcessesToUpdate, System};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReconnectIdentity {
    pub pid: u32,
    pub birth_token: u64,
}

/// Record reconnect identity for a live process, if birth time is available.
pub fn record(pid: u32) -> Option<ReconnectIdentity> {
    let mut system = System::new();
    let sys_pid = Pid::from_u32(pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[sys_pid]), true);
    let process = system.process(sys_pid)?;
    Some(ReconnectIdentity {
        pid,
        birth_token: process.start_time(),
    })
}

/// Verify that `pid` still refers to the same process instance as `birth_token`.
pub fn verify(pid: u32, birth_token: u64) -> bool {
    record(pid).is_some_and(|id| id.birth_token == birth_token)
}

/// Best-effort check that a PID still exists (used when sysinfo cannot see MSYS/bash).
///
/// The Windows fallback used to shell out to `powershell.exe` per call, which costs
/// hundreds of milliseconds to over a second just to spin up the PowerShell host — brutal
/// when this runs once per stale agent/shell on every launch. It now uses `OpenProcess`
/// directly, which is a plain syscall (microseconds).
pub fn pid_exists(pid: u32) -> bool {
    if record(pid).is_some() {
        return true;
    }
    #[cfg(windows)]
    {
        return windows_pid_exists(pid);
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn windows_pid_exists(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_current_process() {
        let pid = std::process::id();
        let id = record(pid).expect("current process should be visible");
        assert_eq!(id.pid, pid);
        assert!(verify(pid, id.birth_token));
        assert!(!verify(pid, id.birth_token.wrapping_add(1)));
    }
}
