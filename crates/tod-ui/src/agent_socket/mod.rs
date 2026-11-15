//! Opt-in agent control socket: synthetic GPUI key/click + screenshots.
//!
//! Line protocol (one command per line, one `ok` / `err …` reply):
//! - `key <keystroke>`
//! - `text <string>` — insert into focused input (after focus click + sync)
//! - `click <x> <y>`
//! - `rclick <x> <y>` — right-click, e.g. to open a list row's context menu
//! - `scroll <x> <y> <dx> <dy>` — wheel scroll at a point; +dy scrolls down
//! - `shot <path> [x0 y0 x1 y1]`
//! - `sync` — wait one UI frame (use before a shot after clicks if paint must settle)
//! - `transcripts open` — open or focus the agent transcript window (single instance)
//! - `transcripts close` — close the transcript window if open
//! - `transcripts focus` — focus the transcript window if open
//! - `transcripts status` — reply `ok open` or `ok closed`
//! - `agent-platform get` — reply `ok claude` or `ok cursor`
//! - `agent-platform cycle` — cycle platform and reply with new value
//! - `agent-platform set cursor|claude` — set platform and reply with new value
//! - `mode` — reply `ok driven|user|idle` (see below)
//! - `size` — reply `ok W H`, the window's content size in logical pixels
//! - `resize W H` — set the content size (the app owns it while driven)
//! - `test-user move X Y | click X Y | key K | resize W H | leave | blur | focus | peek`
//!   — **test-only**: plays an event of the user's through the real input path
//!   (see README)
//!
//! # Sharing the app with the user (driven mode vs user mode)
//!
//! The same instance is the user's. The socket marks its own injected events
//! (`driver::synthetic`), so any input outside that mark is the user's
//! (`driver.rs` says where it is seen). `mode.rs` is the state machine:
//!
//! - `driven`: a client is driving, from its first input command until no
//!   socket command for 2 minutes (then `idle`: no indicator, no snapshots).
//!   The title bar is violet and says "Shepherd driving".
//! - `user`: the person is interacting. Real key, click, scroll, or a pointer
//!   move with a real delta switches to it; focus gained, or the pointer
//!   merely over the window, does not. A key or click that switches is
//!   consumed, so it never lands in a field the agent focused. On the switch
//!   the agent's view state is saved and the user's own last view restored
//!   (and the reverse on switching back): `snapshot.rs` lists what a view
//!   holds. Only view state, never data. Menus are closed on a switch.
//! - Back to `driven` when the window loses key focus or the pointer leaves
//!   it for 300 ms, or when the user stops interacting for `--driven-idle-ms`
//!   (default 5000).
//!
//! Input commands (`key`, `text`, `click`, `rclick`, `scroll`, `drag`,
//! `press`/`moveto`/`release`, `resize`) that arrive in `user` mode **block**
//! until driven mode resumes and the agent's view is restored. Then, if the
//! data it depended on is unchanged, the command runs and replies `ok`; if
//! something changed it replies `err user-conflict: <what changed>` (for
//! example `request 3f2a answered; ENG-2925 pr_draft→approved`) without
//! running; after 60 s it replies `err user-mode` and the agent resends.
//! `sync`, `mode` and `size` run at once in any mode. `shot`/`shot2` in
//! `user` mode reply `err user-mode: …`, since the screen shows the user's
//! view.
//!
//! - `tree [--role R] [--label S] [--within SEL] [--json]` — the UI by identity
//!   (selector, role, label, value, bounds, actions); see `a11y_tree` and `selector`.
//!   Text is in it: a `label` node per string no interactive node names, else that
//!   node's label (a button's label excludes the text of buttons inside it).
//!   Selectors and `act`'s selector accept double quotes (`"textinput:Item title"`)
//! - `act <selector> [click|focus|set-value <text>|scroll-into-view]` — act on one
//!   element through the accessibility action path (synthetic click if it has none);
//!   `set-value` also focuses the field
//! - `wait-for <selector> [--gone] [--timeout-ms N]` — poll until it matches
//! - `coverage [frames N]` — clickable versus exposed elements, optional frame timing
//!
//! `tree`/`act`/`wait-for`/`coverage` read GPUI's AccessKit tree, which the vendored
//! `gpui-pre` (`vendor/README.md`, feature `tod-a11y`, on with `agent-socket`) lets us
//! switch on in-process. The first such command forces it on; `watch_a11y_idle` turns
//! it off after 30 s without one, so an idle socket adds no per-frame work.
//!
//! Keys use GPUI `dispatch_keystroke` (same path as typed input). Clicks use
//! `SendMessage` to this process's HWND — synchronous, no focus steal, no sleep.
//! The UI drain loop wakes on each command (no polling).

mod a11y_tree;
mod capture;
pub mod commands;
mod driver;
pub mod mode;
mod snapshot;

pub use driver::{driving, input_watch};
pub mod selector;

use crate::app::transcript_window::{TranscriptWindowControl, TranscriptWindowStatus};
use commands::{AgentPlatformSocketCommand, Command, TranscriptsCommand, parse_line};
use mode::{Kind, Mode, Shared};
use gpui::{AnyWindowHandle, AsyncApp, Keystroke};
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
    /// Force a draw, render the scene offscreen, write a PNG (see `shot2`).
    Shot2 {
        path: std::path::PathBuf,
        crop: Option<(f32, f32, f32, f32)>,
    },
    Quit,
    /// What the window's close button does: the close guard runs.
    Close,
    /// A synthetic mouse click through GPUI (platforms without a native path).
    #[cfg(not(windows))]
    Click { x: f32, y: f32, right: bool },
    /// A synthetic wheel event at a point (user sense: +dy = scroll down).
    Scroll { x: f32, y: f32, dx: f32, dy: f32 },
    Transcripts(TranscriptsCommand),
    AgentPlatform(AgentPlatformSocketCommand),
    /// Reply `ok W H`: the window's content size.
    Size,
    Resize { w: f32, h: f32 },
    /// Before an agent command runs: put the window back at the size the app
    /// owns if it differs; reply the note to attach (empty for none).
    EnsureSize,
    /// The dev-only `test-user` seam.
    TestUser(commands::TestUser),
    A11yTree(a11y_tree::TreeQuery),
    A11yCoverage { frames: usize },
    A11yAct {
        selector: String,
        action: a11y_tree::ActAction,
    },
    /// How many nodes a selector matches now (what `wait-for` polls).
    A11yCount(String),
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
    _logical_width: f32,
    _logical_height: f32,
    driven_idle_ms: u64,
    transcript_window: TranscriptWindowControl,
    shell: gpui::WeakEntity<crate::app::window::Shell>,
) {
    let shutdown = Arc::new(AtomicBool::new(false));
    let _ = SHUTDOWN.set(shutdown.clone());
    eprintln!("agent-socket: listening on {addr}");
    let (tx, rx) = async_channel::bounded::<Pending>(32);
    let shared = Arc::new(Shared::new(mode::Config {
        idle_ms: driven_idle_ms,
        ..mode::Config::default()
    }));

    let for_threads = shared.clone();
    std::thread::Builder::new()
        .name("tod-agent-socket".into())
        .spawn(move || listen_loop(listener, tx, shutdown, for_threads))
        .expect("spawn agent socket thread");

    // Real vs synthetic input, the mode, snapshots, the title bar.
    let driver = driver::Driver::install(cx, window, shell.clone(), shared);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if window
                .update(cx, |_, window, cx| driver.tick(window, cx))
                .is_err()
            {
                break;
            }
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        drain_loop(cx, window, transcript_window, shell, rx).await;
    })
    .detach();
}

fn listen_loop(
    listener: TcpListener,
    tx: async_channel::Sender<Pending>,
    shutdown: Arc<AtomicBool>,
    shared: Arc<Shared>,
) {
    let _ = listener.set_nonblocking(true);
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let tx = tx.clone();
                let shared = shared.clone();
                let _ = std::thread::Builder::new()
                    .name("tod-agent-client".into())
                    .spawn(move || handle_client(stream, tx, shared));
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

fn handle_client(stream: TcpStream, tx: async_channel::Sender<Pending>, shared: Arc<Shared>) {
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

        // Input commands wait here while the user has the app (`mode.rs`).
        let parsed = parse_line(line.trim_end_matches(['\r', '\n']))
            .and_then(|cmd| gate(&cmd, &shared).map(|()| cmd));
        // The agent's command runs on the window at the size the app owns.
        let mut note: Option<String> = None;
        let parsed = match parsed {
            Ok(cmd)
                if matches!(mode::classify(&cmd), Kind::Input | Kind::Observe)
                    && !matches!(cmd, Command::Resize { .. }) =>
            {
                ensure_size(&tx).map(|n| {
                    note = n;
                    cmd
                })
            }
            other => other,
        };
        let reply = match parsed {
            Ok(Command::TestUser(event)) => dispatch_ui(&tx, UiRequest::TestUser(event)),
            Ok(Command::Shot { path, crop }) => window_size(&tx).and_then(|(w, h)| {
                capture::capture_window_png(&path, w, h, crop).map(|()| "ok".to_string())
            }),
            Ok(Command::Mode) => Ok(format!("ok {}", shared.mode().word())),
            Ok(Command::Size) => dispatch_ui(&tx, UiRequest::Size),
            Ok(Command::Resize { w, h }) => dispatch_ui(&tx, UiRequest::Resize { w, h }),
            Ok(Command::Shot2 { path, crop }) => {
                dispatch_ui(&tx, UiRequest::Shot2 { path, crop })
            }
            Ok(Command::Key { keystroke }) => dispatch_ui(&tx, UiRequest::Key(keystroke)),
            Ok(Command::Text { text }) => dispatch_ui(&tx, UiRequest::Text(text)),
            #[cfg(windows)]
            Ok(Command::Click { x, y, right }) => {
                let _synthetic = driver::synthetic();
                window_size(&tx).and_then(|(w, h)| {
                    capture::send_click(x, y, w, h, right).map(|()| "ok".to_string())
                })
            }
            #[cfg(not(windows))]
            Ok(Command::Click { x, y, right }) => dispatch_ui(&tx, UiRequest::Click { x, y, right }),
            Ok(Command::Scroll { x, y, dx, dy }) => dispatch_ui(&tx, UiRequest::Scroll { x, y, dx, dy }),
            Ok(Command::Close) => dispatch_ui(&tx, UiRequest::Close),
            Ok(Command::Drag { from, to }) => {
                let _synthetic = driver::synthetic();
                window_size(&tx).and_then(|(w, h)| {
                    capture::send_drag(from, to, w, h).map(|()| "ok".to_string())
                })
            }
            Ok(Command::DragStep { step, x, y }) => {
                let _synthetic = driver::synthetic();
                window_size(&tx).and_then(|(w, h)| {
                    capture::send_drag_step(step, x, y, w, h).map(|()| "ok".to_string())
                })
            }
            Ok(Command::Sync) => dispatch_ui(&tx, UiRequest::Sync),
            Ok(Command::Quit) => dispatch_ui(&tx, UiRequest::Quit),
            Ok(Command::Transcripts(action)) => dispatch_ui(&tx, UiRequest::Transcripts(action)),
            Ok(Command::AgentPlatform(action)) => {
                dispatch_ui(&tx, UiRequest::AgentPlatform(action))
            }
            Ok(Command::Coverage { frames }) => dispatch_ui(&tx, UiRequest::A11yCoverage { frames }),
            Ok(Command::Tree(query)) => dispatch_ui(&tx, UiRequest::A11yTree(query)),
            Ok(Command::Act { selector, action }) => {
                dispatch_ui(&tx, UiRequest::A11yAct { selector, action })
            }
            Ok(Command::WaitFor { selector, gone, timeout_ms }) => {
                wait_for(&tx, &selector, gone, timeout_ms)
            }
            Err(e) => Err(e),
        };

        let msg = match reply {
            Ok(line) => match note {
                // On the first line, so a multi-line reply (`tree`) keeps its
                // announced line count.
                Some(note) => match line.split_once('\n') {
                    Some((first, rest)) => format!("{first} # note: {note}\n{rest}\n"),
                    None => format!("{line} # note: {note}\n"),
                },
                None => format!("{line}\n"),
            },
            Err(e) => format!("err {e}\n"),
        };
        if writer.write_all(msg.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

/// Ask the UI thread to put the window at the size the app owns before an
/// agent command runs. The platform reports a resize a moment after it is
/// made, so ask again (bounded) while it says so; a note comes back when the
/// platform would not give the size.
fn ensure_size(tx: &async_channel::Sender<Pending>) -> Result<Option<String>, String> {
    for _ in 0..20 {
        let reply = dispatch_ui(tx, UiRequest::EnsureSize)?;
        if reply != driver::ENSURE_WAIT {
            return Ok(Some(reply).filter(|n| !n.is_empty()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("the window did not settle at its size".into())
}

/// Decide whether a command may run now (`mode::classify`): an input command
/// waits for the user to be done, a screenshot is refused while they are in
/// control. `Err` is the text after `err `.
fn gate(command: &Command, shared: &Shared) -> Result<(), String> {
    let kind = mode::classify(command);
    let activity = mode::counts_as_activity(kind);
    match kind {
        Kind::Input | Kind::Observe => shared.gate_wait(activity),
        Kind::Shot if shared.mode() == Mode::User => Err(
            "user-mode: the screen shows the user's view; retry when `mode` says driven".into(),
        ),
        Kind::Shot | Kind::Seam | Kind::Read | Kind::Other => {
            if activity {
                shared.with(|m, now| m.note_command(now));
            }
            Ok(())
        }
    }
}

/// The window's content size now, which `resize` and the user can change.
fn window_size(tx: &async_channel::Sender<Pending>) -> Result<(u32, u32), String> {
    let reply = dispatch_ui(tx, UiRequest::Size)?;
    let mut parts = reply.split_whitespace().skip(1);
    let mut next = || {
        parts
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or_else(|| format!("unreadable size reply `{reply}`"))
    };
    Ok((next()?, next()?))
}

/// Poll the tree until the selector matches (or stops matching). Runs on the
/// client's own thread, so the UI is free between polls.
fn wait_for(
    tx: &async_channel::Sender<Pending>,
    selector: &str,
    gone: bool,
    timeout_ms: u64,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let count: usize = dispatch_ui(tx, UiRequest::A11yCount(selector.to_string()))?
            .parse()
            .map_err(|_| "agent UI returned a bad count".to_string())?;
        if (count > 0) != gone {
            return Ok(if gone {
                "ok gone".to_string()
            } else {
                format!("ok {count} matches")
            });
        }
        if std::time::Instant::now() >= deadline {
            return Err(if gone {
                format!("timeout {selector} still matches ({count})")
            } else {
                format!("timeout {selector} never matched")
            });
        }
        std::thread::sleep(Duration::from_millis(50));
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
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            Ok("ok".into())
        } else {
            handle_ui_request(cx, window, &transcript_window, &shell, pending.request)
        };
        let _ = pending.reply.send(result);

        while let Ok(pending) = rx.try_recv() {
            let result = if matches!(pending.request, UiRequest::Sync) {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
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
    // The test seam plays the user: no synthetic guard.
    if let UiRequest::TestUser(event) = request {
        return window
            .update(cx, |_root, window, cx| match driver::of(cx) {
                Some(driver) => driver.test_user(event, window, cx),
                None => Err("no driver installed".to_string()),
            })
            .map_err(|e| format!("window update failed: {e}"))?;
    }
    // Everything the socket does to the window is synthetic: input seen
    // meanwhile is not the user's (`driver::synthetic`).
    let _synthetic = driver::synthetic();
    match request {
        UiRequest::Sync => Ok("ok".into()),
        UiRequest::Size => window
            .update(cx, |_root, window, _| {
                let size = window.viewport_size();
                format!(
                    "ok {} {}",
                    f32::from(size.width).round() as u32,
                    f32::from(size.height).round() as u32
                )
            })
            .map_err(|e| format!("window update failed: {e}")),
        UiRequest::Resize { w, h } => window
            .update(cx, |_root, window, cx| match driver::of(cx) {
                Some(driver) => {
                    driver.agent_resize(window, (w, h));
                    Ok("ok".to_string())
                }
                None => Err("no driver installed".to_string()),
            })
            .map_err(|e| format!("window update failed: {e}"))?,
        UiRequest::EnsureSize => window
            .update(cx, |_root, window, cx| match driver::of(cx) {
                Some(driver) => Ok(driver.ensure_size(window)),
                None => Err("no driver installed".to_string()),
            })
            .map_err(|e| format!("window update failed: {e}"))?,
        UiRequest::TestUser(_) => unreachable!("handled above"),
        UiRequest::Shot2 { path, crop } => window
            .update(cx, |_root, window, cx| {
                // Force a fresh layout+paint of the current state: an occluded
                // window gets no display-link frames, so its last scene is stale.
                window.refresh();
                let _ = window.draw(cx);
                let rgba = window.render_to_image().map_err(|e| format!("render_to_image: {e}"))?;
                let size = window.viewport_size();
                let (lw, lh) = (f32::from(size.width).round() as u32, f32::from(size.height).round() as u32);
                capture::write_scaled(&path, rgba, lw, lh, crop)?;
                Ok("ok".to_string())
            })
            .map_err(|e| format!("window update failed: {e}"))?,
        UiRequest::Quit => Ok(cx
            .update(|app| {
                // Closing the last window quits the app (`app::mod`), the
                // same path as the user closing it.
                for window in app.windows() {
                    let _ = window.update(app, |_, window, _| window.remove_window());
                }
                "ok".to_string()
            })),
        UiRequest::Close => window
            .update(cx, |_root, window, cx| {
                let allowed = crate::app::window::request_close(window, cx);
                if allowed { window.remove_window(); "ok closed".to_string() } else { "ok blocked".to_string() }
            })
            .map_err(|e| format!("close failed: {e}")),
        #[cfg(not(windows))]
        UiRequest::Click { x, y, right } => window
            .update(cx, |_root, window, cx| {
                use gpui::{px, point, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput};
                let position = point(px(x), px(y));
                let button = if right { MouseButton::Right } else { MouseButton::Left };
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent { button, position, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent { button, position, modifiers: Modifiers::default(), click_count: 1 }),
                    cx,
                );
                "ok".to_string()
            })
            .map_err(|e| format!("click failed: {e}")),
        UiRequest::Scroll { x, y, dx, dy } => window
            .update(cx, |_root, window, cx| {
                use gpui::{point, px, Modifiers, MouseMoveEvent, PlatformInput, ScrollDelta, ScrollWheelEvent, TouchPhase};
                let position = point(px(x), px(y));
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
                    cx,
                );
                // GPUI deltas follow the scroll offset (negative = content moves
                // up), so the user-facing "+dy = down" is negated here.
                window.dispatch_event(
                    PlatformInput::ScrollWheel(ScrollWheelEvent {
                        position,
                        delta: ScrollDelta::Pixels(point(px(-dx), px(-dy))),
                        modifiers: Modifiers::default(),
                        touch_phase: TouchPhase::Moved,
                    }),
                    cx,
                );
                "ok".to_string()
            })
            .map_err(|e| format!("scroll failed: {e}")),
        UiRequest::A11yTree(query) => {
            let reply = window
                .update(cx, |_root, window, cx| a11y_tree::run_tree(window, cx, &query))
                .map_err(|e| format!("window update failed: {e}"))?;
            watch_a11y_idle(cx, window);
            reply
        }
        UiRequest::A11yCoverage { frames } => {
            let reply = window
                .update(cx, |_root, window, cx| a11y_tree::run_coverage(window, cx, frames))
                .map_err(|e| format!("window update failed: {e}"))?;
            watch_a11y_idle(cx, window);
            reply
        }
        UiRequest::A11yAct { selector, action } => {
            let reply = window
                .update(cx, |_root, window, cx| {
                    a11y_tree::run_act(window, cx, &selector, &action)
                })
                .map_err(|e| format!("window update failed: {e}"))?;
            watch_a11y_idle(cx, window);
            reply
        }
        UiRequest::A11yCount(selector) => {
            let count = window
                .update(cx, |_root, window, cx| a11y_tree::count(window, cx, &selector))
                .map_err(|e| format!("window update failed: {e}"))?;
            watch_a11y_idle(cx, window);
            count.map(|n| n.to_string())
        }
        UiRequest::Key(keystroke) => apply_key(cx, window, keystroke).map(|()| "ok".into()),
        UiRequest::Text(text) => apply_text(cx, window, text).map(|()| "ok".into()),
        UiRequest::Transcripts(action) => apply_transcripts(cx, transcript_window, action),
        UiRequest::AgentPlatform(action) => apply_agent_platform(cx, shell, action),
    }
}

static A11Y_WATCHING: AtomicBool = AtomicBool::new(false);

/// The accessibility tree is only built while commands use it: after
/// [`a11y_tree::IDLE_OFF`] without one, switch it off again.
fn watch_a11y_idle(cx: &mut AsyncApp, window: AnyWindowHandle) {
    if A11Y_WATCHING.swap(true, Ordering::Relaxed) {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            if a11y_tree::idle_for() < a11y_tree::IDLE_OFF {
                continue;
            }
            let _ = window.update(cx, |_root, window, _cx| a11y_tree::release(window));
            A11Y_WATCHING.store(false, Ordering::Relaxed);
            break;
        }
    })
    .detach();
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
            // Prefer direct insert into gpui-component's focused input when available.
            use gpui_component::input::AnyInputState;
            match window.focused_input(cx) {
                Some(AnyInputState::Input(input)) => {
                    input.update(cx, |state, cx| state.insert(text.clone(), window, cx));
                    return Ok(());
                }
                Some(AnyInputState::Textarea(input)) => {
                    input.update(cx, |state, cx| state.insert(text.clone(), window, cx));
                    return Ok(());
                }
                Some(_) => return Err("focused input does not accept inserted text".into()),
                None => {}
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

    /// The gate on a clock the test owns: what the socket threads call.
    fn gated() -> (Shared, std::sync::Arc<std::sync::atomic::AtomicU64>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let clock = std::sync::Arc::new(AtomicU64::new(0));
        let c = clock.clone();
        let shared = Shared::with_clock(mode::Config::default(), move || c.load(Ordering::SeqCst));
        assert_eq!(shared.mode(), Mode::Idle);
        (shared, clock)
    }

    fn send(shared: &Shared, line: &str) -> Result<(), String> {
        gate(&parse_line(line).unwrap(), shared)
    }

    /// A client sends `line` every 10 s for five minutes after one input
    /// command; returns the mode when it stops (ticking as the app does).
    fn poll_for_five_minutes(line: &str) -> Mode {
        use std::sync::atomic::Ordering;
        let (shared, clock) = gated();
        send(&shared, "click 1 1").unwrap();
        assert_eq!(shared.mode(), Mode::Driven);
        let mut t = 10_000;
        while t <= 300_000 {
            clock.store(t, Ordering::SeqCst);
            send(&shared, line).unwrap();
            shared.with(|m, now| m.tick(now));
            t += 10_000;
        }
        shared.mode()
    }

    #[test]
    fn a_client_polling_observations_goes_idle_through_the_gate() {
        for line in ["tree", "wait-for button:Go --timeout-ms 10", "coverage", "mode", "sync", "size", "shot /x.png"]
        {
            // `wait-for` is gated once on arrival; the poll itself is not.
            assert_eq!(poll_for_five_minutes(line), Mode::Idle, "{line}");
        }
    }

    #[test]
    fn a_client_acting_stays_driven_through_the_gate() {
        for line in ["act button:Go", "key a", "text hi", "click 1 1", "quit"] {
            assert_eq!(poll_for_five_minutes(line), Mode::Driven, "{line}");
        }
    }

    #[test]
    fn observing_from_idle_does_not_start_driving() {
        let (shared, _) = gated();
        for line in ["tree", "wait-for button:Go", "coverage"] {
            send(&shared, line).unwrap();
            assert_eq!(shared.mode(), Mode::Idle, "{line}");
        }
        send(&shared, "act button:Go").unwrap();
        assert_eq!(shared.mode(), Mode::Driven);
    }

    #[test]
    fn observing_waits_while_the_user_has_the_app() {
        use std::sync::atomic::Ordering;
        let (shared, clock) = gated();
        let shared = std::sync::Arc::new(shared);
        send(&shared, "click 1 1").unwrap();
        clock.store(1_000, Ordering::SeqCst);
        shared.with(|m, now| m.on_real_input(now, mode::RealInput::Key));
        assert_eq!(shared.mode(), Mode::User);
        let waiter = {
            let shared = shared.clone();
            std::thread::spawn(move || send(&shared, "tree"))
        };
        std::thread::sleep(Duration::from_millis(150));
        assert!(!waiter.is_finished(), "tree must wait in user mode");
        // The user goes quiet; driven mode resumes and the view is restored.
        clock.store(7_000, Ordering::SeqCst);
        assert!(shared.with(|m, now| m.tick(now)).is_some());
        shared.with(|m, _| m.restored(vec![]));
        assert_eq!(waiter.join().unwrap(), Ok(()));
    }

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
