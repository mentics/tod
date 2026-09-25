//! An interactive terminal on a sandbox, through the relay.
//!
//! The shell runs in a relay session, so the socket can come and go: after
//! `park_after` with no input, no output, and no foreground job, the socket is
//! closed (the sandbox may then sleep) and the next keypress reattaches. A
//! running build or server keeps the terminal attached, and the relay holds the
//! sandbox awake for it. `tod-cli` in the terminal works while it is attached
//! (typing reattaches it), through the relay's tunnel.

use crate::relay::{self, Event, ExecRequest, Ws};
use anyhow::{Result, bail};
use futures_util::{SinkExt, StreamExt};
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub struct TerminalOptions {
    /// Command to run instead of a login shell.
    pub cmd: Option<String>,
    pub cwd: Option<String>,
    /// The sandbox's URL, for the tunnel.
    pub sandbox_url: String,
    /// Environment for the shell.
    pub env: std::collections::HashMap<String, String>,
    /// Carry the relay's tunnel to this local port while attached, so
    /// `tod-cli` in the terminal reaches the app.
    pub tunnel_port: Option<u16>,
    /// Park an idle terminal after this long; `None` never parks.
    pub park_after: Option<Duration>,
    /// Called with a line about each attach and park, for a log.
    pub log: fn(&str),
}

pub async fn run(url: &str, token: &str, opts: TerminalOptions) -> Result<i32> {
    let _raw = raw::enable();
    let session = format!(
        "term-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
    );
    let mut size = raw::size();
    let first = ExecRequest {
        cmd: opts.cmd.clone(),
        session: Some(session.clone()),
        pty: Some(size),
        cwd: opts.cwd.clone(),
        env: opts.env.clone(),
        ..ExecRequest::default()
    };
    // The tunnel is open exactly while the terminal is attached.
    let tunnel = |on: bool, current: &mut Option<tokio::task::JoinHandle<()>>| {
        if let Some(t) = current.take() {
            t.abort();
        }
        if let (true, Some(port)) = (on, opts.tunnel_port) {
            let (url, token) = (opts.sandbox_url.clone(), token.to_string());
            *current = Some(tokio::spawn(async move {
                loop {
                    let _ = crate::tunnel::run(&url, &token, port, None).await;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }));
        }
    };
    let mut tunnel_task = None;

    // Keystrokes, from a blocking reader.
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if key_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut ws: Option<Ws> = Some(attach(url, token, &first).await?);
    tunnel(true, &mut tunnel_task);
    (opts.log)(&format!("terminal {session}: attached"));
    let mut stdout = std::io::stdout();
    let mut busy = false;
    let mut last_activity = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut stdin_open = true;

    loop {
        tokio::select! {
            keys = key_rx.recv(), if stdin_open => {
                let Some(keys) = keys else {
                    stdin_open = false;
                    continue;
                };
                last_activity = Instant::now();
                if ws.is_none() {
                    let mut s = attach(url, token, &ExecRequest::reattach(&session)).await?;
                    s.send(relay::resize_message(size.0, size.1)).await?;
                    (opts.log)(&format!("terminal {session}: reattached on input"));
                    ws = Some(s);
                    tunnel(true, &mut tunnel_task);
                }
                if ws.as_mut().unwrap().send(Message::binary(keys)).await.is_err() {
                    ws = None;
                }
            }
            msg = async { ws.as_mut().unwrap().next().await }, if ws.is_some() => {
                let Some(Ok(msg)) = msg else {
                    (opts.log)(&format!("terminal {session}: socket dropped"));
                    ws = None;
                    tunnel(false, &mut tunnel_task);
                    continue;
                };
                match relay::parse(msg) {
                    Event::Stdout(b) => {
                        last_activity = Instant::now();
                        stdout.write_all(&b)?;
                        stdout.flush()?;
                    }
                    Event::Stderr(s) => {
                        // Only the relay itself writes here for a terminal.
                        stdout.write_all(s.replace('\n', "\r\n").as_bytes())?;
                        stdout.flush()?;
                    }
                    Event::Busy(b) => busy = b,
                    Event::Exit(code) => return Ok(code),
                    Event::Closed => {
                        ws = None;
                        tunnel(false, &mut tunnel_task);
                    }
                    Event::Other => {}
                }
            }
            _ = tick.tick() => {
                let now = raw::size();
                if now != size {
                    size = now;
                    if let Some(s) = ws.as_mut() {
                        let _ = s.send(relay::resize_message(size.0, size.1)).await;
                    }
                }
                if let (Some(park), Some(s)) = (opts.park_after, ws.as_mut()) {
                    if !busy && last_activity.elapsed() >= park {
                        let _ = s.close(None).await;
                        ws = None;
                        tunnel(false, &mut tunnel_task);
                        (opts.log)(&format!("terminal {session}: parked"));
                    }
                }
            }
        }
    }
}

async fn attach(url: &str, token: &str, req: &ExecRequest) -> Result<Ws> {
    let mut ws = relay::connect(url, token).await?;
    ws.send(req.message()).await?;
    Ok(ws)
}

/// Puts the local terminal in raw mode for as long as the guard lives.
pub mod raw {
    pub struct Guard {
        #[cfg(unix)]
        saved: Option<libc::termios>,
        #[cfg(windows)]
        saved: Option<(u32, u32)>,
    }

    #[cfg(unix)]
    pub fn enable() -> Guard {
        // SAFETY: termios calls on fd 0 with a zeroed struct they fill in.
        unsafe {
            if libc::isatty(0) == 0 {
                return Guard { saved: None };
            }
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return Guard { saved: None };
            }
            let saved = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(0, libc::TCSANOW, &t);
            Guard { saved: Some(saved) }
        }
    }

    #[cfg(unix)]
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(t) = self.saved {
                // SAFETY: restores the attributes read in `enable`.
                unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
            }
        }
    }

    #[cfg(unix)]
    pub fn size() -> (u16, u16) {
        // SAFETY: TIOCGWINSZ fills in the winsize.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
                return (ws.ws_col, ws.ws_row);
            }
        }
        (80, 24)
    }

    #[cfg(windows)]
    pub fn enable() -> Guard {
        use windows_sys::Win32::System::Console::*;
        // SAFETY: console mode calls on the process's own standard handles.
        unsafe {
            let (hin, hout) = (GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE));
            let (mut min, mut mout) = (0u32, 0u32);
            if GetConsoleMode(hin, &mut min) == 0 || GetConsoleMode(hout, &mut mout) == 0 {
                return Guard { saved: None };
            }
            let raw_in = (min & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            let raw_out = mout | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
            SetConsoleMode(hin, raw_in);
            SetConsoleMode(hout, raw_out);
            Guard { saved: Some((min, mout)) }
        }
    }

    #[cfg(windows)]
    impl Drop for Guard {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Console::*;
            if let Some((min, mout)) = self.saved {
                // SAFETY: restores the modes read in `enable`.
                unsafe {
                    SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), min);
                    SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), mout);
                }
            }
        }
    }

    #[cfg(windows)]
    pub fn size() -> (u16, u16) {
        use windows_sys::Win32::System::Console::*;
        // SAFETY: fills in a screen-buffer info struct for our own stdout.
        unsafe {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) != 0 {
                let w = info.srWindow;
                return ((w.Right - w.Left + 1) as u16, (w.Bottom - w.Top + 1) as u16);
            }
        }
        (80, 24)
    }
}

/// Fails fast with a readable message when stdin is not interactive.
pub fn require_terminal() -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!("not a terminal: run this from an interactive shell");
    }
    Ok(())
}
