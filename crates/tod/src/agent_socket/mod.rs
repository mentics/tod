//! Opt-in agent control socket: synthetic GPUI key/click + screenshots.
//!
//! Line protocol (one command per line, one `ok` / `err …` reply):
//! - `key <keystroke>`
//! - `text <string>` — insert into focused input (after focus click + sync)
//! - `click <x> <y>`
//! - `shot <path> [x0 y0 x1 y1]`
//! - `sync` — wait one UI frame (use before a shot after clicks if paint must settle)
//! - `transcripts open` — open or focus the agent transcript window (single instance)
//! - `transcripts close` — close the transcript window if open
//! - `transcripts focus` — focus the transcript window if open
//! - `transcripts status` — reply `ok open` or `ok closed`
//! - `agent-platform get` — reply `ok claude` or `ok cursor`
//! - `agent-platform cycle` — cycle platform and reply with new value
//! - `agent-platform set cursor|claude` — set platform and reply with new value
//!
//! Keys use GPUI `dispatch_keystroke` (same path as typed input). Clicks use
//! `SendMessage` to this process's HWND — synchronous, no focus steal, no sleep.
//! The UI drain loop wakes on each command (no polling).

mod capture;
pub mod commands;

use crate::app::transcript_window::{TranscriptWindowControl, TranscriptWindowStatus};
use commands::{AgentPlatformSocketCommand, Command, TranscriptsCommand, parse_line};
use gpui::{AnyWindowHandle, AsyncApp, Keystroke, Timer};
use gpui_component::WindowExt;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

static SHUTDOWN: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

/// Stop the accept loop so the process can exit after the last window closes.
pub fn shutdown() {
    if let Some(flag) = SHUTDOWN.get() {
        flag.store(true, Ordering::Relaxed);
    }
}

/// Bind the control socket. Call before opening the main window so port conflicts fail fast.
pub fn bind(addr: SocketAddr) -> anyhow::Result<TcpListener> {
    TcpListener::bind(addr).map_err(|err| {
        anyhow::anyhow!("agent-socket: bind {addr} failed: {err} (choose another port)")
    })
}

enum UiRequest {
    Key(String),
    /// Insert characters into the focused GPUI text input handler.
    Text(String),
    /// Yield one frame so layout/paint after input is visible to `shot`.
    Sync,
    Transcripts(TranscriptsCommand),
    AgentPlatform(AgentPlatformSocketCommand),
}

struct Pending {
    request: UiRequest,
    reply: SyncSender<Result<String, String>>,
}

/// Start accept loop + UI drain loop after the main window is open.
/// `listener` must come from [`bind`] on the requested address.
pub fn start(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    listener: TcpListener,
    addr: SocketAddr,
    logical_width: f32,
    logical_height: f32,
    transcript_window: TranscriptWindowControl,
    shell: gpui::WeakEntity<crate::app::window::Shell>,
) {
    let shutdown = Arc::new(AtomicBool::new(false));
    let _ = SHUTDOWN.set(shutdown.clone());
    eprintln!("agent-socket: listening on {addr}");
    let (tx, rx) = async_channel::bounded::<Pending>(32);
    let lw = logical_width.round().max(1.0) as u32;
    let lh = logical_height.round().max(1.0) as u32;

    std::thread::Builder::new()
        .name("tod-agent-socket".into())
        .spawn(move || listen_loop(listener, tx, lw, lh, shutdown))
        .expect("spawn agent socket thread");

    cx.spawn(async move |cx| {
        drain_loop(cx, window, transcript_window, shell, rx).await;
    })
    .detach();
}

fn listen_loop(
    listener: TcpListener,
    tx: async_channel::Sender<Pending>,
    logical_width: u32,
    logical_height: u32,
    shutdown: Arc<AtomicBool>,
) {
    let _ = listener.set_nonblocking(true);
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let tx = tx.clone();
                let _ = std::thread::Builder::new()
                    .name("tod-agent-client".into())
                    .spawn(move || handle_client(stream, tx, logical_width, logical_height));
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("agent-socket: accept error: {e}");
                break;
            }
        }
    }
}

fn handle_client(
    stream: TcpStream,
    tx: async_channel::Sender<Pending>,
    logical_width: u32,
    logical_height: u32,
) {
    // Accepted sockets can inherit the listener's non-blocking mode (notably on
    // Windows). Force blocking so read_line waits for the next command instead
    // of treating WouldBlock as a disconnect.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let mut reader = BufReader::new(stream.try_clone().expect("clone tcp stream"));
    let mut writer = stream;
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("agent-socket: read error: {e}");
                break;
            }
        }

        let reply = match parse_line(line.trim_end_matches(['\r', '\n'])) {
            Ok(Command::Shot { path, crop }) => {
                capture::capture_window_png(&path, logical_width, logical_height, crop)
                    .map(|()| "ok".to_string())
            }
            Ok(Command::Key { keystroke }) => dispatch_ui(&tx, UiRequest::Key(keystroke)),
            Ok(Command::Text { text }) => dispatch_ui(&tx, UiRequest::Text(text)),
            Ok(Command::Click { x, y }) => {
                capture::send_click(x, y, logical_width, logical_height).map(|()| "ok".to_string())
            }
            Ok(Command::Sync) => dispatch_ui(&tx, UiRequest::Sync),
            Ok(Command::Transcripts(action)) => dispatch_ui(&tx, UiRequest::Transcripts(action)),
            Ok(Command::AgentPlatform(action)) => {
                dispatch_ui(&tx, UiRequest::AgentPlatform(action))
            }
            Err(e) => Err(e),
        };

        let msg = match reply {
            Ok(line) => format!("{line}\n"),
            Err(e) => format!("err {e}\n"),
        };
        if writer.write_all(msg.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

fn dispatch_ui(tx: &async_channel::Sender<Pending>, request: UiRequest) -> Result<String, String> {
    dispatch_ui_with_timeout(tx, request, Duration::from_secs(5))
}

fn dispatch_ui_with_timeout(
    tx: &async_channel::Sender<Pending>,
    request: UiRequest,
    timeout: Duration,
) -> Result<String, String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    tx.send_blocking(Pending {
        request,
        reply: reply_tx,
    })
    .map_err(|_| "agent UI channel closed".to_string())?;
    reply_rx
        .recv_timeout(timeout)
        .map_err(|_| "agent UI command timed out".to_string())?
}

async fn drain_loop(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    transcript_window: TranscriptWindowControl,
    shell: gpui::WeakEntity<crate::app::window::Shell>,
    rx: async_channel::Receiver<Pending>,
) {
    while let Ok(pending) = rx.recv().await {
        let result = if matches!(pending.request, UiRequest::Sync) {
            Timer::after(Duration::from_millis(16)).await;
            Ok("ok".into())
        } else {
            handle_ui_request(cx, window, &transcript_window, &shell, pending.request)
        };
        let _ = pending.reply.send(result);

        while let Ok(pending) = rx.try_recv() {
            let result = if matches!(pending.request, UiRequest::Sync) {
                Timer::after(Duration::from_millis(16)).await;
                Ok("ok".into())
            } else {
                handle_ui_request(cx, window, &transcript_window, &shell, pending.request)
            };
            let _ = pending.reply.send(result);
        }
    }
}

fn handle_ui_request(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    transcript_window: &TranscriptWindowControl,
    shell: &gpui::WeakEntity<crate::app::window::Shell>,
    request: UiRequest,
) -> Result<String, String> {
    match request {
        UiRequest::Sync => Ok("ok".into()),
        UiRequest::Key(keystroke) => apply_key(cx, window, keystroke).map(|()| "ok".into()),
        UiRequest::Text(text) => apply_text(cx, window, text).map(|()| "ok".into()),
        UiRequest::Transcripts(action) => apply_transcripts(cx, transcript_window, action),
        UiRequest::AgentPlatform(action) => apply_agent_platform(cx, shell, action),
    }
}

fn apply_agent_platform(
    cx: &mut AsyncApp,
    shell: &gpui::WeakEntity<crate::app::window::Shell>,
    action: AgentPlatformSocketCommand,
) -> Result<String, String> {
    shell
        .update(cx, |shell, cx| {
            shell.handle_agent_platform_socket(action, cx)
        })
        .map_err(|err| format!("shell update failed: {err}"))?
}

fn apply_transcripts(
    cx: &mut AsyncApp,
    transcript_window: &TranscriptWindowControl,
    action: TranscriptsCommand,
) -> Result<String, String> {
    cx.update(|app| match action {
        TranscriptsCommand::Open => transcript_window.open_or_focus(app).map(|()| "ok".into()),
        TranscriptsCommand::Close => transcript_window.close(app).map(|()| "ok".into()),
        TranscriptsCommand::Focus => transcript_window.focus_if_open(app).map(|()| "ok".into()),
        TranscriptsCommand::Status => Ok(match transcript_window.status(app) {
            TranscriptWindowStatus::Open => "ok open".into(),
            TranscriptWindowStatus::Closed => "ok closed".into(),
        }),
    })
    .map_err(|err| format!("app update failed: {err}"))?
}

fn apply_key(cx: &mut AsyncApp, window: AnyWindowHandle, keystroke: String) -> Result<(), String> {
    window
        .update(cx, |_root, window, cx| {
            let ks = Keystroke::parse(&keystroke)
                .map_err(|e| format!("invalid keystroke `{keystroke}`: {e}"))?;
            let _handled = window.dispatch_keystroke(ks, cx);
            Ok(())
        })
        .map_err(|e| format!("window update failed: {e}"))?
}

fn apply_text(cx: &mut AsyncApp, window: AnyWindowHandle, text: String) -> Result<(), String> {
    window
        .update(cx, |_root, window, cx| {
            // Prefer direct insert into gpui-component focused InputState when available.
            if let Some(input) = window.focused_input(cx) {
                input.update(cx, |state, cx| {
                    state.insert(text.clone(), window, cx);
                });
                return Ok(());
            }
            Err(
                "no focused input — click the field (or key ctrl-shift-n for Notes) then sync"
                    .into(),
            )
        })
        .map_err(|e| format!("window update failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_fails_when_port_in_use() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let err = bind(addr).unwrap_err();
        assert!(err.to_string().contains("bind"));
        assert!(err.to_string().contains("choose another port"));
    }
}
