//! A process on a pseudo-terminal, for interactive shells.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub struct Pty {
    pub master: File,
    pub child: Child,
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

pub fn spawn(mut cmd: Command, cols: u16, rows: u16) -> std::io::Result<Pty> {
    let (mut master, mut slave) = (0, 0);
    let ws = winsize(cols, rows);
    // SAFETY: openpty writes two fds into the out-params; the winsize is read-only.
    if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &ws) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both fds were just returned by openpty and are owned here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    cmd.stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok(Pty { master: File::from(master), child })
}

pub fn resize(master: &File, cols: u16, rows: u16) {
    let ws = winsize(cols, rows);
    // SAFETY: TIOCSWINSZ reads a winsize from the pointer.
    unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) };
}

/// True while a job other than the shell itself owns the terminal.
pub fn foreground_job(master: &File, shell_pid: u32) -> bool {
    // SAFETY: tcgetpgrp only reads the fd's foreground process group.
    let pgrp = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    pgrp > 0 && pgrp as u32 != shell_pid
}

pub fn write_all(mut master: &File, bytes: &[u8]) {
    let _ = master.write_all(bytes);
}
