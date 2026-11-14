//! The UI-thread side of driven/user mode: telling the user's input from the
//! socket's, feeding `mode::Machine`, and carrying out its transitions
//! (snapshots, indicator).
//!
//! **Real vs synthetic.** The socket injects events with
//! `Window::dispatch_event` / `dispatch_keystroke` on the UI thread, inside a
//! [`synthetic`] guard (a process-wide counter, so the Windows paths, which
//! `SendMessage` from the socket thread, are covered too). Everything the
//! platform delivers reaches the same `dispatch_event`, so an event seen
//! while no guard is held is the user's. Hooks, none of which touch GPUI:
//! - keys: `App::intercept_keystrokes`, which runs before any binding or
//!   input handler, so `stop_propagation` consumes the key;
//! - mouse: capture-phase listeners (`Window::on_mouse_event`) registered by
//!   [`input_watch`], the first child of the shell, so they run before the
//!   views' own handlers and `stop_propagation` consumes the click;
//! - focus: `Window::observe_window_activation`;
//! - size: `Context::observe_window_bounds`, compared with the size the app
//!   owns (see below).
//!
//! **Size.** While driven, the app owns the window's content size
//! (`expected`: what the agent's `resize` set, or the last restore). Every
//! resize the app makes itself goes through [`Driver::own_resize`], which
//! ignores the platform's callback for a short while after (it may arrive
//! after the `synthetic` guard is dropped). Any other size change is the
//! user's (an edge drag, a tiling tool, a display change):
//! [`RealInput::Resize`]. In user mode the user's size is theirs
//! (`user_size`); on return to driven the agent's size is put back.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, DispatchPhase, Global, IntoElement, MouseDownEvent,
    MouseExitEvent, MouseMoveEvent, MouseUpEvent, ScrollWheelEvent, Styled, WeakEntity, Window,
    canvas,
};

use super::commands::TestUser;
use super::mode::{Effect, Mode, RealInput, Shared};
use super::snapshot::{self, Snapshots};
use crate::app::window::Shell;

static SYNTHETIC: AtomicUsize = AtomicUsize::new(0);
static DRIVING_SHOWN: AtomicBool = AtomicBool::new(false);

/// Held while the socket injects an event: input seen meanwhile is not the user's.
pub struct Synthetic(());

pub fn synthetic() -> Synthetic {
    SYNTHETIC.fetch_add(1, Ordering::SeqCst);
    Synthetic(())
}

impl Drop for Synthetic {
    fn drop(&mut self) {
        SYNTHETIC.fetch_sub(1, Ordering::SeqCst);
    }
}

fn synthetic_active() -> bool {
    SYNTHETIC.load(Ordering::SeqCst) > 0
}

/// Whether the title bar shows "Shepherd driving" (driven mode).
pub fn driving() -> bool {
    DRIVING_SHOWN.load(Ordering::Relaxed)
}

pub struct Driver {
    pub shared: Arc<Shared>,
    window: AnyWindowHandle,
    shell: WeakEntity<Shell>,
    snapshots: RefCell<Snapshots>,
    shown: Cell<bool>,
    /// The content size the app owns while driven (logical pixels).
    expected: Cell<(f32, f32)>,
    /// The size last seen by the bounds observer.
    seen: Cell<(f32, f32)>,
    /// The user's own size, kept while they have the app.
    user_size: Cell<Option<(f32, f32)>>,
    /// The app's own resize may still be reported until then.
    own_until: Cell<Option<Instant>>,
    /// A size the app asked for, checked at the next command (a clamp).
    wanted: Cell<Option<(f32, f32)>>,
    /// Keeps the bounds observer alive.
    watch: RefCell<Option<gpui::Entity<()>>>,
    /// The switch to user mode now being made came from a resize.
    resize_cause: Cell<bool>,
}

/// How long after the app's own resize a size change is still taken as its own.
/// What `ensure_size` replies while the platform has not yet reported a resize.
pub const ENSURE_WAIT: &str = "~";

const OWN_RESIZE_WINDOW: Duration = Duration::from_millis(300);

fn close(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 1.0 && (a.1 - b.1).abs() < 1.0
}

fn viewport(window: &Window) -> (f32, f32) {
    let v = window.viewport_size();
    (f32::from(v.width), f32::from(v.height))
}

fn did_not_fit(got: (f32, f32), want: (f32, f32)) -> String {
    format!(
        "window is {}x{}, the size {}x{} did not fit",
        got.0.round(),
        got.1.round(),
        want.0.round(),
        want.1.round()
    )
}

/// The driver of the app's main window, when the agent socket is on.
pub fn of(cx: &App) -> Option<Rc<Driver>> {
    cx.try_global::<DriverGlobal>().map(|g| g.0.clone())
}

struct DriverGlobal(Rc<Driver>);
impl Global for DriverGlobal {}

impl Driver {
    pub fn install(
        cx: &mut AsyncApp,
        window: AnyWindowHandle,
        shell: WeakEntity<Shell>,
        shared: Arc<Shared>,
    ) -> Rc<Driver> {
        let start = window
            .update(cx, |_, window, _| viewport(window))
            .unwrap_or((0.0, 0.0));
        let driver = Rc::new(Driver {
            shared,
            window,
            shell,
            snapshots: RefCell::new(Snapshots::default()),
            shown: Cell::new(false),
            expected: Cell::new(start),
            seen: Cell::new(start),
            user_size: Cell::new(None),
            own_until: Cell::new(None),
            wanted: Cell::new(None),
            resize_cause: Cell::new(false),
            watch: RefCell::new(None),
        });
        let hook = driver.clone();
        let _ = cx.update(|app| {
            app.set_global(DriverGlobal(hook.clone()));
            let keys = hook.clone();
            app.intercept_keystrokes(move |_event, window, cx| {
                if keys.is_main(window) && keys.real_input(RealInput::Key, window, cx) {
                    cx.stop_propagation();
                }
            })
            .detach();
        });
        let focus = driver.clone();
        let bounds = driver.clone();
        let observer_shell = driver.shell.clone();
        let _ = window.update(cx, |_, window, cx| {
            let _ = observer_shell.update(cx, |_, cx| {
                cx.observe_window_activation(window, move |_, window, _| {
                    focus.on_activation(window.is_window_active());
                })
                .detach();
            });
            // Not on the shell: the callback may switch mode, which reads the
            // shell, and gpui refuses a read of an entity being updated.
            let watch = cx.new(|_| ());
            let hook = bounds.clone();
            watch.update(cx, |_, cx| {
                cx.observe_window_bounds(window, move |_, window, cx| {
                    hook.on_bounds(window, cx);
                })
                .detach();
            });
            *bounds.watch.borrow_mut() = Some(watch);
        });
        driver
    }

    /// The window gained or lost key focus (what the activation observer
    /// reports; the `test-user blur|focus` seam calls it too).
    fn on_activation(&self, active: bool) {
        self.shared.with(|m, now| m.on_focus(now, active));
    }

    /// Resize as the app. The platform reports the new size a moment later
    /// (`window.resize` does not update `viewport_size` itself), so the size
    /// asked for is what the app owns from here; the report is taken as the
    /// app's own for [`OWN_RESIZE_WINDOW`] and, if the platform clamped it,
    /// corrects the owned size (see [`Driver::on_bounds`]).
    fn own_resize(&self, window: &mut Window, to: (f32, f32)) {
        let _own = synthetic();
        if !close(viewport(window), to) {
            self.own_until.set(Some(Instant::now() + OWN_RESIZE_WINDOW));
            window.resize(gpui::size(gpui::px(to.0), gpui::px(to.1)));
        }
        self.seen.set(to);
        if self.shared.mode() == Mode::User {
            self.user_size.set(Some(to));
        } else {
            self.expected.set(to);
        }
    }

    /// A size change reported by the platform.
    fn on_bounds(&self, window: &mut Window, cx: &mut App) {
        let now = viewport(window);
        if close(now, self.seen.get()) {
            return;
        }
        self.seen.set(now);
        let own = synthetic_active() || self.own_until.get().is_some_and(|t| Instant::now() < t);
        let mode = self.shared.mode();
        if own {
            if mode == Mode::User {
                self.user_size.set(Some(now));
            } else {
                self.expected.set(now);
            }
            return;
        }
        match mode {
            Mode::Idle => {
                self.expected.set(now);
                self.user_size.set(Some(now));
            }
            Mode::Driven => {
                self.resize_cause.set(true);
                self.real_input(RealInput::Resize, window, cx);
                self.resize_cause.set(false);
            }
            Mode::User => {
                self.user_size.set(Some(now));
                self.real_input(RealInput::Resize, window, cx);
            }
        }
    }

    /// Before an agent command runs: in driven mode the window must be the
    /// size the app owns (a change that slipped past the observer is undone
    /// here). Replies [`ENSURE_WAIT`] while the platform is still reporting
    /// the app's own resize (ask again), else a short note when it would not
    /// give the size asked for (a screen too small), else nothing.
    pub fn ensure_size(&self, window: &mut Window) -> String {
        if self.own_until.get().is_some_and(|t| Instant::now() < t) {
            return ENSURE_WAIT.into();
        }
        let got = viewport(window);
        let mode = self.shared.mode();
        if let Some(want) = self.wanted.take() {
            if !close(got, want) {
                // Accept what the platform gave; say so once.
                if mode == Mode::Driven {
                    self.expected.set(got);
                }
                return did_not_fit(got, want);
            }
        }
        if mode == Mode::Driven {
            let want = self.expected.get();
            if !close(got, want) {
                self.wanted.set(Some(want));
                self.own_resize(window, want);
                return ENSURE_WAIT.into();
            }
        }
        String::new()
    }

    /// The agent's `resize`: this is the size the app now owns. If the
    /// platform clamps it, the next command's note says so.
    pub fn agent_resize(&self, window: &mut Window, to: (f32, f32)) {
        self.wanted.set(Some(to));
        self.own_resize(window, to);
    }

    /// Play an event of the user's (dev-only `test-user` seam): delivered
    /// with no `synthetic` guard, as the platform would.
    pub fn test_user(&self, event: TestUser, window: &mut Window, cx: &mut App) -> Result<String, String> {
        use gpui::{KeyDownEvent, Keystroke, Modifiers, MouseButton, PlatformInput, point, px};
        let mouse_move = |x: f32, y: f32| {
            PlatformInput::MouseMove(MouseMoveEvent {
                position: point(px(x), px(y)),
                pressed_button: None,
                modifiers: Modifiers::default(),
            })
        };
        match event {
            TestUser::Move { x, y } => {
                window.dispatch_event(mouse_move(x, y), cx);
            }
            TestUser::Click { x, y } => {
                let position = point(px(x), px(y));
                window.dispatch_event(mouse_move(x, y), cx);
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    }),
                    cx,
                );
            }
            TestUser::Key(key) => {
                let keystroke = Keystroke::parse(&key)
                    .map_err(|e| format!("invalid keystroke `{key}`: {e}"))?;
                window.dispatch_event(
                    PlatformInput::KeyDown(KeyDownEvent {
                        keystroke,
                        is_held: false,
                        prefer_character_input: false,
                    }),
                    cx,
                );
            }
            TestUser::Resize { w, h } => {
                // Not `own_resize`: the app did not do this. The platform
                // reports it afterwards, as it does a drag of the window edge.
                window.resize(gpui::size(px(w), px(h)));
            }
            TestUser::Leave => {
                window.dispatch_event(
                    PlatformInput::MouseExited(MouseExitEvent {
                        position: point(px(-1.0), px(-1.0)),
                        pressed_button: None,
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
            }
            TestUser::Blur => self.on_activation(false),
            TestUser::Focus => self.on_activation(true),
            TestUser::Peek => {
                let view = snapshot::capture(&self.shell, window, cx)?;
                return Ok(format!(
                    "ok mode={} {}",
                    self.shared.mode().word(),
                    view.summary()
                ));
            }
        }
        Ok("ok".into())
    }

    fn is_main(&self, window: &Window) -> bool {
        snapshot::is_window(self.window, window)
    }

    /// A key, a button, a scroll or a pointer move, from wherever. `true`
    /// means the event is to be consumed.
    fn real_input(&self, input: RealInput, window: &mut Window, cx: &mut App) -> bool {
        if synthetic_active() {
            return false;
        }
        let (consume, effect) = self.shared.with(|m, now| m.on_real_input(now, input));
        if let Some(effect) = effect {
            self.apply(effect, window, cx);
        }
        consume
    }

    /// Time passing: the debounce, the idle timeout, the agent going quiet;
    /// and the title bar following the mode.
    pub fn tick(&self, window: &mut Window, cx: &mut App) {
        if let Some(effect) = self.shared.with(|m, now| m.tick(now)) {
            self.apply(effect, window, cx);
        }
        let driven = self.shared.mode() == Mode::Driven;
        if driven != self.shown.get() {
            self.shown.set(driven);
            DRIVING_SHOWN.store(driven, Ordering::Relaxed);
            let _ = self.shell.update(cx, |_, cx| cx.notify());
        }
    }

    fn apply(&self, effect: Effect, window: &mut Window, cx: &mut App) {
        match effect {
            Effect::EnterUser => {
                let from_resize = self.resize_cause.get();
                // The agent's view, at the size the app owns (the window may
                // already be at the user's new one).
                let agent = snapshot::capture(&self.shell, window, cx)
                    .map(|a| a.with_size(self.expected.get()));
                let user = self.snapshots.borrow().user.clone();
                self.snapshots.borrow_mut().agent = Some(agent.clone());
                // Their own last view if they have one; else the view as the
                // agent left it, minus the field it was typing in.
                let has_user_view = user.is_some();
                let target = user.or_else(|| agent.ok());
                if from_resize {
                    self.user_size.set(Some(viewport(window)));
                }
                if let Some(target) = target {
                    let size = if from_resize {
                        viewport(window)
                    } else {
                        self.user_size.get().unwrap_or(target.size())
                    };
                    self.own_resize(window, size);
                    if let Err(err) =
                        snapshot::restore(&self.shell, &target, has_user_view, window, cx)
                    {
                        eprintln!("agent-socket: restoring the user's view failed: {err}");
                    }
                }
            }
            Effect::EnterDriven => {
                let user = snapshot::capture(&self.shell, window, cx);
                self.user_size.set(Some(viewport(window)));
                let agent = self.snapshots.borrow_mut().agent.take();
                let mut conflicts = Vec::new();
                match agent {
                    Some(Ok(agent)) => {
                        match snapshot::fingerprint_now(&self.shell, &agent, cx) {
                            Ok(now) => conflicts = agent.fingerprint.conflicts(&now),
                            Err(err) => conflicts.push(format!("could not compare the data: {err}")),
                        }
                        // The agent's size first, then its view.
                        self.wanted.set(Some(agent.size()));
                        self.own_resize(window, agent.size());
                        if let Err(err) = snapshot::restore(&self.shell, &agent, true, window, cx) {
                            conflicts.push(format!("could not restore the agent's view: {err}"));
                        }
                    }
                    Some(Err(err)) => {
                        conflicts.push(format!("the agent's view was not saved: {err}"));
                        let want = self.expected.get();
                        self.own_resize(window, want);
                    }
                    None => {}
                }
                match user {
                    Ok(user) => self.snapshots.borrow_mut().user = Some(user),
                    Err(err) => eprintln!("agent-socket: saving the user's view failed: {err}"),
                }
                self.shared.with(|m, _| m.restored(conflicts));
            }
            Effect::EnterIdle => {
                *self.snapshots.borrow_mut() = Snapshots::default();
                let now = viewport(window);
                self.expected.set(now);
                self.user_size.set(Some(now));
            }
        }
    }
}

/// An invisible element whose capture-phase listeners tell the user's mouse
/// from the socket's. Put it first in the shell's root so it runs first.
pub fn input_watch() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |_, _, window, cx| {
            let Some(driver) = cx.try_global::<DriverGlobal>().map(|g| g.0.clone()) else {
                return;
            };
            let d = driver.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                let _ = event;
                if phase == DispatchPhase::Capture && d.real_input(RealInput::Button, window, cx) {
                    cx.stop_propagation();
                }
            });
            let d = driver.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                let _ = event;
                if phase == DispatchPhase::Capture && d.real_input(RealInput::Button, window, cx) {
                    cx.stop_propagation();
                }
            });
            let d = driver.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    let input = RealInput::PointerMove {
                        x: f32::from(event.position.x),
                        y: f32::from(event.position.y),
                    };
                    d.real_input(input, window, cx);
                }
            });
            let d = driver.clone();
            window.on_mouse_event(move |_: &ScrollWheelEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    d.real_input(RealInput::Scroll, window, cx);
                }
            });
            let d = driver;
            window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    d.real_input(RealInput::PointerExit, window, cx);
                }
            });
        },
    )
    .absolute()
    .w_0()
    .h_0()
}
