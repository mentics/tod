//! Opt-in agent control socket: synthetic GPUI key/click + screenshots.
//!
//! Line protocol (one command per line, one `ok` / `err …` reply):
//! - `key <keystroke>`
//! - `click <x> <y>`
//! - `shot <path> [x0 y0 x1 y1]`
//!
//! Keys use GPUI `dispatch_keystroke` (same path as typed input). Clicks use
//! `PostMessage` to this process's HWND — GPUI's `dispatch_event` returns a
//! crate-private type and cannot be called from outside `gpui`.

mod capture;
mod commands;
mod options;

pub use options::LaunchOptions;

use commands::{parse_line, Command};
use gpui::{AnyWindowHandle, AsyncApp, Keystroke, Timer};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

enum UiRequest {
    Key(String),
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
    let (tx, rx) = mpsc::sync_channel::<Pending>(8);
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
    tx: SyncSender<Pending>,
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
    tx: SyncSender<Pending>,
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
            Ok(Command::Key { keystroke }) => {
                dispatch_ui(&tx, UiRequest::Key(keystroke))
            }
            Ok(Command::Click { x, y }) => {
                capture::post_click(x, y, logical_width, logical_height)
            }
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

fn dispatch_ui(tx: &SyncSender<Pending>, request: UiRequest) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    tx.send(Pending {
        request,
        reply: reply_tx,
    })
    .map_err(|_| "agent UI channel closed".to_string())?;
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "agent UI command timed out".to_string())?
}

async fn drain_loop(cx: &mut AsyncApp, window: AnyWindowHandle, rx: Receiver<Pending>) {
    loop {
        while let Ok(pending) = rx.try_recv() {
            let result = apply_on_ui(cx, window, pending.request);
            let _ = pending.reply.send(result);
        }
        Timer::after(Duration::from_millis(16)).await;
    }
}

fn apply_on_ui(cx: &mut AsyncApp, window: AnyWindowHandle, request: UiRequest) -> Result<(), String> {
    window
        .update(cx, |_root, window, cx| match request {
            UiRequest::Key(keystroke) => {
                let ks = Keystroke::parse(&keystroke)
                    .map_err(|e| format!("invalid keystroke `{keystroke}`: {e}"))?;
                let _handled = window.dispatch_keystroke(ks, cx);
                Ok(())
            }
        })
        .map_err(|e| format!("window update failed: {e}"))?
}
