//! Opt-in agent control socket: synthetic GPUI key/click + screenshots.
//!
//! Line protocol (one command per line, one `ok` / `err …` reply):
//! - `key <keystroke>`
//! - `text <string>` — insert into focused input (after focus click + sync)
//! - `click <x> <y>`
//! - `shot <path> [x0 y0 x1 y1]`
//! - `sync` — wait one UI frame (use before a shot after clicks if paint must settle)
//!
//! Keys use GPUI `dispatch_keystroke` (same path as typed input). Clicks use
//! `SendMessage` to this process's HWND — synchronous, no focus steal, no sleep.
//! The UI drain loop wakes on each command (no polling).

mod capture;
mod commands;
mod options;

pub use options::LaunchOptions;

use commands::{Command, parse_line};
use gpui::{AnyWindowHandle, AsyncApp, Keystroke, Timer};
use gpui_component::WindowExt;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

enum UiRequest {
    Key(String),
    /// Insert characters into the focused GPUI text input handler.
    Text(String),
    /// Yield one frame so layout/paint after input is visible to `shot`.
    Sync,
}

struct Pending {
    request: UiRequest,
    reply: SyncSender<Result<(), String>>,
}

/// Start TCP listener + UI drain loop after the main window is open.
pub fn start(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    addr: SocketAddr,
    logical_width: f32,
    logical_height: f32,
) {
    let (tx, rx) = async_channel::bounded::<Pending>(32);
    let lw = logical_width.round().max(1.0) as u32;
    let lh = logical_height.round().max(1.0) as u32;

    std::thread::Builder::new()
        .name("tod-agent-socket".into())
        .spawn(move || listen_loop(addr, tx, lw, lh))
        .expect("spawn agent socket thread");

    cx.spawn(async move |cx| {
        drain_loop(cx, window, rx).await;
    })
    .detach();
}

fn listen_loop(
    addr: SocketAddr,
    tx: async_channel::Sender<Pending>,
    logical_width: u32,
    logical_height: u32,
) {
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("agent-socket: bind {addr} failed: {e}");
            return;
        }
    };
    eprintln!("agent-socket: listening on {addr}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let tx = tx.clone();
                let _ = std::thread::Builder::new()
                    .name("tod-agent-client".into())
                    .spawn(move || handle_client(stream, tx, logical_width, logical_height));
            }
            Err(e) => eprintln!("agent-socket: accept error: {e}"),
        }
    }
}

fn handle_client(
    stream: TcpStream,
    tx: async_channel::Sender<Pending>,
    logical_width: u32,
    logical_height: u32,
) {
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
            }
            Ok(Command::Key { keystroke }) => dispatch_ui(&tx, UiRequest::Key(keystroke)),
            Ok(Command::Text { text }) => dispatch_ui(&tx, UiRequest::Text(text)),
            Ok(Command::Click { x, y }) => capture::send_click(x, y, logical_width, logical_height),
            Ok(Command::Sync) => dispatch_ui(&tx, UiRequest::Sync),
            Err(e) => Err(e),
        };

        let msg = match reply {
            Ok(()) => "ok\n".to_string(),
            Err(e) => format!("err {e}\n"),
        };
        if writer.write_all(msg.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

fn dispatch_ui(tx: &async_channel::Sender<Pending>, request: UiRequest) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    tx.send_blocking(Pending {
        request,
        reply: reply_tx,
    })
    .map_err(|_| "agent UI channel closed".to_string())?;
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "agent UI command timed out".to_string())?
}

async fn drain_loop(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    rx: async_channel::Receiver<Pending>,
) {
    while let Ok(pending) = rx.recv().await {
        let result = match pending.request {
            UiRequest::Sync => {
                Timer::after(Duration::from_millis(16)).await;
                Ok(())
            }
            UiRequest::Key(keystroke) => apply_key(cx, window, keystroke),
            UiRequest::Text(text) => apply_text(cx, window, text),
        };
        let _ = pending.reply.send(result);

        while let Ok(pending) = rx.try_recv() {
            let result = match pending.request {
                UiRequest::Sync => {
                    Timer::after(Duration::from_millis(16)).await;
                    Ok(())
                }
                UiRequest::Key(keystroke) => apply_key(cx, window, keystroke),
                UiRequest::Text(text) => apply_text(cx, window, text),
            };
            let _ = pending.reply.send(result);
        }
    }
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
