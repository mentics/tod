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
//! - focus: `Window::observe_window_activation`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gpui::{
    AnyWindowHandle, App, AsyncApp, DispatchPhase, Global, IntoElement, MouseDownEvent,
    MouseExitEvent, MouseMoveEvent, MouseUpEvent, ScrollWheelEvent, Styled, WeakEntity, Window,
    canvas,
};

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
        let driver = Rc::new(Driver {
            shared,
            window,
            shell,
            snapshots: RefCell::new(Snapshots::default()),
            shown: Cell::new(false),
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
        let observer_shell = driver.shell.clone();
        let _ = window.update(cx, |_, window, cx| {
            let _ = observer_shell.update(cx, |_, cx| {
                cx.observe_window_activation(window, move |_, window, _| {
                    focus
                        .shared
                        .with(|m, now| m.on_focus(now, window.is_window_active()));
                })
                .detach();
            });
        });
        driver
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
                let agent = snapshot::capture(&self.shell, window, cx);
                let user = self.snapshots.borrow().user.clone();
                {
                    let mut snaps = self.snapshots.borrow_mut();
                    snaps.agent = Some(agent.clone());
                }
                // Their own last view if they have one; else the view as the
                // agent left it, minus the field it was typing in.
                let has_user_view = user.is_some();
                let target = user.or_else(|| agent.ok());
                if let Some(target) = target {
                    if let Err(err) =
                        snapshot::restore(&self.shell, &target, has_user_view, window, cx)
                    {
                        eprintln!("agent-socket: restoring the user's view failed: {err}");
                    }
                }
            }
            Effect::EnterDriven => {
                let user = snapshot::capture(&self.shell, window, cx);
                let agent = self.snapshots.borrow_mut().agent.take();
                let mut conflicts = Vec::new();
                match agent {
                    Some(Ok(agent)) => {
                        match snapshot::fingerprint_now(&self.shell, &agent, cx) {
                            Ok(now) => conflicts = agent.fingerprint.conflicts(&now),
                            Err(err) => conflicts.push(format!("could not compare the data: {err}")),
                        }
                        if let Err(err) = snapshot::restore(&self.shell, &agent, true, window, cx) {
                            conflicts.push(format!("could not restore the agent's view: {err}"));
                        }
                    }
                    Some(Err(err)) => {
                        conflicts.push(format!("the agent's view was not saved: {err}"));
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
