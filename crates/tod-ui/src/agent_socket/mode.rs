//! Driven mode vs user mode: how the app shares itself between a socket
//! client (the UI shepherd agent) and the person using the same instance.
//!
//! This file is the pure part: a state machine with no window and no real
//! clock (every call takes `now` in milliseconds), so the rules are unit
//! tested by feeding it events and time. `driver.rs` feeds it real events;
//! `snapshot.rs` does what its [`Effect`]s ask (save and restore the view).
//!
//! Modes:
//! - `Idle`: no agent is driving (none yet, or no socket command for
//!   [`Config::agent_idle_ms`]). Nothing is consumed, nothing is saved.
//! - `Driven`: a client is driving, from its first input command. Real input
//!   from the user switches to `User`.
//! - `User`: the person is interacting. The agent's view was saved and the
//!   person's own last view restored. Input commands wait (see [`Ticket`]).
//!
//! Real interaction (a key, a mouse button, a scroll, or a pointer move of
//! at least [`POINTER_DELTA_PX`] from where the pointer last counted) switches
//! `Driven` to `User`. Focus gained, or the pointer only being over the
//! window, never does: swiping through the Space the window is on gains and
//! loses focus within milliseconds and driven work must go on untouched.
//! A key or a click that switches is consumed, so it never lands in a field
//! the agent focused; a move or a scroll just switches.
//!
//! `User` returns to `Driven` when the user leaves (the window loses key
//! focus, or the pointer leaves it) and stays away for
//! [`Config::leave_debounce_ms`], or when the user stays but does nothing for
//! [`Config::idle_ms`].

use super::commands::Command;

pub type Ms = u64;

/// How a socket command relates to the user's use of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Changes what the window shows or holds: waits while the user has the
    /// app (see [`Ticket`]) and starts driving.
    Input,
    /// Reads the accessibility tree (`tree`, `wait-for`, `coverage`): the tree
    /// describes the screen, which in user mode is the user's view, so it waits
    /// for the agent's view to be restored exactly as [`Kind::Input`] does. But
    /// it only observes, so it neither keeps the agent "driving" nor starts
    /// driving from `Idle`: a client polling `tree`/`wait-for` goes quiet like
    /// one polling `mode`. (`wait-for` only polls the tree's match count; it
    /// never acts. `act` is the one tree command that changes the app and is
    /// an `Input`.)
    Observe,
    /// Looks at the screen: refused while the user has the app, since the
    /// screen shows their view.
    Shot,
    /// Answers from state: runs at once in any mode.
    Read,
    /// Acts on the app but not through the window's input (quit, close, the
    /// transcript window, the agent platform): runs at once.
    Other,
    /// The dev-only `test-user` seam: plays the *user's* input. It is not the
    /// agent's, so it neither waits nor keeps the agent "driving".
    Seam,
}

/// A new `Command` variant is an [`Kind::Input`] until someone says otherwise:
/// the safe mistake is a command that waits, not one that runs on the
/// user's view.
pub fn classify(command: &Command) -> Kind {
    match command {
        Command::Shot { .. } | Command::Shot2 { .. } => Kind::Shot,
        Command::Sync | Command::Mode | Command::Size => Kind::Read,
        Command::TestUser(_) => Kind::Seam,
        Command::Quit
        | Command::Close
        | Command::Transcripts(_)
        | Command::AgentPlatform(_) => Kind::Other,
        Command::Tree(_) | Command::WaitFor { .. } | Command::Coverage { .. } => Kind::Observe,
        Command::Act { .. } => Kind::Input,
        _ => Kind::Input,
    }
}

/// Whether a command of this kind keeps the agent "driving". Looking at the
/// app (`mode`, `sync`, `size`, `shot`, and the tree reads `tree`, `wait-for`,
/// `coverage`) does not: a client polling to ask whether the agent has gone
/// quiet, or waiting for the screen between actions, would otherwise keep it
/// from ever going quiet (the app read `driven` over four minutes after the
/// last input). Input commands (including `act`) and the commands that act on
/// the app do. `mod.rs::gate` is the only caller: the rule lives here, the
/// gate applies it to every command.
pub fn counts_as_activity(kind: Kind) -> bool {
    matches!(kind, Kind::Input | Kind::Other)
}

/// A pointer move counts as the user's only when it is at least this far
/// (logical pixels) from where the pointer last counted; a mouse sitting
/// still, or sensor jitter, never switches.
pub const POINTER_DELTA_PX: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Driven,
    User,
}

impl Mode {
    /// The word `mode` replies with.
    pub fn word(self) -> &'static str {
        match self {
            Mode::Idle => "idle",
            Mode::Driven => "driven",
            Mode::User => "user",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// In `User` mode, no real interaction for this long: back to `Driven`.
    pub idle_ms: Ms,
    /// The user must have left (focus lost, pointer out) this long.
    pub leave_debounce_ms: Ms,
    /// No socket command for this long: the agent is gone, `Idle`.
    pub agent_idle_ms: Ms,
    /// How long an input command waits in `User` mode before `user-mode`.
    pub block_cap_ms: Ms,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_ms: 5_000,
            leave_debounce_ms: 300,
            agent_idle_ms: 120_000,
            block_cap_ms: 60_000,
        }
    }
}

/// An event that did not come from the socket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RealInput {
    Key,
    /// A mouse button going down or up.
    Button,
    Scroll,
    /// The pointer moved to a position in the window (logical pixels).
    PointerMove { x: f32, y: f32 },
    /// The pointer left the window.
    PointerExit,
    /// The window's content size changed and the app did not do it (the
    /// user dragging an edge, a tiling tool, a display change).
    Resize,
}

/// What the app must do because of a transition. The mode has already
/// changed when the effect is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Save the agent's view, restore the user's last one.
    EnterUser,
    /// Save the user's view, restore the agent's, and report how the data
    /// differs ([`Machine::restored`]).
    EnterDriven,
    /// The agent is gone: drop both snapshots, restore nothing.
    EnterIdle,
}

/// An input command waiting for its turn.
#[derive(Debug, Clone, Copy)]
pub struct Ticket {
    arrived: Ms,
    generation: u64,
    /// Only observing: waits like an input but never notes activity or starts
    /// driving.
    observe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Not yet: the user is in control, or the agent's view is being put back.
    Pending,
    /// Run the command and reply `ok`.
    Run,
    /// The agent's view is back but the data it depended on changed: reply
    /// `err user-conflict: <this>` without running.
    Conflict(String),
    /// The user held on for the whole cap: reply `err user-mode`.
    UserMode,
}

const RESTORE_LOG: usize = 32;

#[derive(Debug)]
pub struct Machine {
    cfg: Config,
    mode: Mode,
    last_socket: Option<Ms>,
    last_real: Ms,
    leave_at: Option<Ms>,
    /// Where the real pointer last counted (see [`POINTER_DELTA_PX`]).
    pointer_anchor: Option<(f32, f32)>,
    /// The switch to `Driven` has been decided but its restore not reported.
    restoring: bool,
    generation: u64,
    /// Restores reported, newest last: generation and the conflicts found.
    restore_log: Vec<(u64, Vec<String>)>,
}

impl Machine {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            mode: Mode::Idle,
            last_socket: None,
            last_real: 0,
            leave_at: None,
            pointer_anchor: None,
            restoring: false,
            generation: 0,
            restore_log: Vec::new(),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Any socket command arrived (it keeps the agent "driving").
    pub fn note_command(&mut self, now: Ms) {
        self.last_socket = Some(now);
    }

    /// An input command arrived. From `Idle` this starts driving. Poll the
    /// ticket until it says what to do.
    pub fn begin_input(&mut self, now: Ms) -> Ticket {
        self.begin(now, false)
    }

    /// An observing command (`tree`, `wait-for`, `coverage`) arrived: it waits
    /// for its turn as an input does, but is not agent activity.
    pub fn begin_observe(&mut self, now: Ms) -> Ticket {
        self.begin(now, true)
    }

    fn begin(&mut self, now: Ms, observe: bool) -> Ticket {
        if !observe {
            self.note_command(now);
            if self.mode == Mode::Idle {
                self.mode = Mode::Driven;
            }
        }
        Ticket {
            arrived: now,
            generation: self.generation,
            observe,
        }
    }

    pub fn poll(&mut self, ticket: Ticket, now: Ms) -> Poll {
        if self.mode == Mode::User {
            if now.saturating_sub(ticket.arrived) >= self.cfg.block_cap_ms {
                if !ticket.observe {
                    self.note_command(now);
                }
                return Poll::UserMode;
            }
            return Poll::Pending;
        }
        if self.restoring {
            return Poll::Pending;
        }
        if !ticket.observe {
            self.note_command(now);
            if self.mode == Mode::Idle {
                self.mode = Mode::Driven;
            }
        }
        let mut conflicts: Vec<String> = Vec::new();
        for (generation, found) in &self.restore_log {
            if *generation > ticket.generation {
                for text in found {
                    if !conflicts.contains(text) {
                        conflicts.push(text.clone());
                    }
                }
            }
        }
        if conflicts.is_empty() {
            Poll::Run
        } else {
            Poll::Conflict(conflicts.join("; "))
        }
    }

    /// The restore a [`Effect::EnterDriven`] asked for is done; `conflicts`
    /// lists what changed in the data the agent depended on.
    pub fn restored(&mut self, conflicts: Vec<String>) {
        self.restoring = false;
        self.generation += 1;
        self.restore_log.push((self.generation, conflicts));
        if self.restore_log.len() > RESTORE_LOG {
            self.restore_log.remove(0);
        }
    }

    /// A real event. Returns whether to swallow it, and a switch to carry out.
    pub fn on_real_input(&mut self, now: Ms, input: RealInput) -> (bool, Option<Effect>) {
        let interaction = match input {
            RealInput::Key | RealInput::Button | RealInput::Scroll | RealInput::Resize => true,
            RealInput::PointerMove { x, y } => self.pointer_delta(x, y),
            RealInput::PointerExit => {
                if self.mode == Mode::User && self.leave_at.is_none() {
                    self.leave_at = Some(now + self.cfg.leave_debounce_ms);
                }
                return (false, None);
            }
        };
        if !interaction {
            return (false, None);
        }
        match self.mode {
            Mode::Idle => (false, None),
            Mode::Driven => {
                self.mode = Mode::User;
                self.last_real = now;
                self.leave_at = None;
                let consume = matches!(input, RealInput::Key | RealInput::Button);
                (consume, Some(Effect::EnterUser))
            }
            Mode::User => {
                self.last_real = now;
                self.leave_at = None;
                (false, None)
            }
        }
    }

    fn pointer_delta(&mut self, x: f32, y: f32) -> bool {
        match self.pointer_anchor {
            None => {
                self.pointer_anchor = Some((x, y));
                false
            }
            Some((ax, ay)) => {
                if (x - ax).abs().max((y - ay).abs()) >= POINTER_DELTA_PX {
                    self.pointer_anchor = Some((x, y));
                    true
                } else {
                    false
                }
            }
        }
    }

    /// The window gained or lost key focus. Gaining never switches to `User`;
    /// in `User` mode, losing it starts the leave debounce and gaining it
    /// again cancels it.
    pub fn on_focus(&mut self, now: Ms, active: bool) {
        if self.mode != Mode::User {
            return;
        }
        if active {
            self.leave_at = None;
        } else if self.leave_at.is_none() {
            self.leave_at = Some(now + self.cfg.leave_debounce_ms);
        }
    }

    /// Time passing: the leave debounce, the idle timeout, the agent going quiet.
    pub fn tick(&mut self, now: Ms) -> Option<Effect> {
        if self.mode != Mode::Idle {
            if let Some(last) = self.last_socket {
                if now.saturating_sub(last) >= self.cfg.agent_idle_ms {
                    self.mode = Mode::Idle;
                    self.leave_at = None;
                    self.restoring = false;
                    return Some(Effect::EnterIdle);
                }
            }
        }
        if self.mode == Mode::User {
            let left = self.leave_at.is_some_and(|at| now >= at);
            let idle = now.saturating_sub(self.last_real) >= self.cfg.idle_ms;
            if left || idle {
                self.mode = Mode::Driven;
                self.leave_at = None;
                self.restoring = true;
                return Some(Effect::EnterDriven);
            }
        }
        None
    }
}

/// The machine shared between the UI thread (real events, ticks) and the
/// socket threads (commands), with a real clock.
pub struct Shared {
    machine: std::sync::Mutex<Machine>,
    clock: Box<dyn Fn() -> Ms + Send + Sync>,
}

impl Shared {
    pub fn new(cfg: Config) -> Self {
        let epoch = std::time::Instant::now();
        Self::with_clock(cfg, move || epoch.elapsed().as_millis() as Ms)
    }

    /// A shared machine on a clock the caller owns (tests drive time by hand).
    pub fn with_clock(cfg: Config, clock: impl Fn() -> Ms + Send + Sync + 'static) -> Self {
        Self {
            machine: std::sync::Mutex::new(Machine::new(cfg)),
            clock: Box::new(clock),
        }
    }

    pub fn now(&self) -> Ms {
        (self.clock)()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Machine, Ms) -> R) -> R {
        let now = self.now();
        let mut machine = self.machine.lock().expect("mode machine mutex");
        f(&mut machine, now)
    }

    pub fn mode(&self) -> Mode {
        self.with(|m, _| m.mode())
    }

    /// Wait for the turn of an input command. `Ok` means run it; the error
    /// is the text after `err `.
    pub fn gate_wait(&self, activity: bool) -> Result<(), String> {
        let ticket = self.with(|m, now| {
            if activity {
                m.begin_input(now)
            } else {
                m.begin_observe(now)
            }
        });
        loop {
            match self.with(|m, now| m.poll(ticket, now)) {
                Poll::Run => return Ok(()),
                Poll::Conflict(what) => return Err(format!("user-conflict: {what}")),
                Poll::UserMode => return Err("user-mode".into()),
                Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Machine {
        Machine::new(Config::default())
    }

    /// An agent that has sent its first input command at t=0.
    fn driven() -> Machine {
        let mut m = machine();
        let t = m.begin_input(0);
        assert_eq!(m.poll(t, 0), Poll::Run);
        assert_eq!(m.mode(), Mode::Driven);
        m
    }

    #[test]
    fn commands_are_classified() {
        use super::super::commands::parse_line;
        let kind = |line: &str| classify(&parse_line(line).unwrap());
        for line in [
            "key a", "text hi", "click 1 2", "rclick 1 2", "scroll 1 2 3 4", "drag 1 2 3 4",
            "press 1 2", "moveto 1 2", "release 1 2", "resize 800 600",
        ] {
            assert_eq!(kind(line), Kind::Input, "{line}");
        }
        assert_eq!(kind("shot /tmp/x.png"), Kind::Shot);
        assert_eq!(kind("shot2 /tmp/x.png"), Kind::Shot);
        for line in ["sync", "mode", "size"] {
            assert_eq!(kind(line), Kind::Read, "{line}");
        }
        for line in ["quit", "close", "transcripts status", "agent-platform get"] {
            assert_eq!(kind(line), Kind::Other, "{line}");
        }
    }

    #[test]
    fn the_test_user_seam_is_neither_agent_input_nor_a_read() {
        use super::super::commands::parse_line;
        for line in ["test-user move 1 2", "test-user click 1 2", "test-user key a", "test-user resize 900 600", "test-user leave", "test-user blur", "test-user focus", "test-user peek"] {
            assert_eq!(classify(&parse_line(line).unwrap()), Kind::Seam, "{line}");
        }
    }

    #[test]
    fn a_resize_the_app_did_not_make_switches_without_being_consumed() {
        let mut m = driven();
        assert_eq!(m.on_real_input(100, RealInput::Resize), (false, Some(Effect::EnterUser)));
        assert_eq!(m.mode(), Mode::User);
    }

    #[test]
    fn a_resize_while_idle_is_nothing() {
        let mut m = machine();
        assert_eq!(m.on_real_input(100, RealInput::Resize), (false, None));
        assert_eq!(m.mode(), Mode::Idle);
    }

    #[test]
    fn a_resize_in_user_mode_counts_as_activity_and_cancels_the_leave() {
        let mut m = driven();
        m.on_real_input(100, RealInput::Resize);
        m.on_real_input(110, RealInput::PointerExit);
        // Still dragging the edge: activity, so the leave is cancelled.
        assert_eq!(m.on_real_input(200, RealInput::Resize), (false, None));
        assert_eq!(m.tick(500), None);
        assert_eq!(m.mode(), Mode::User);
        // Quiet for idle_ms: back to driven, restore pending.
        assert_eq!(m.tick(200 + Config::default().idle_ms), Some(Effect::EnterDriven));
        let t = m.begin_input(200 + Config::default().idle_ms);
        assert_eq!(m.poll(t, 200 + Config::default().idle_ms), Poll::Pending);
        m.restored(vec![]);
        assert_eq!(m.poll(t, 6_000), Poll::Run);
    }

    #[test]
    fn starts_idle_and_real_input_does_nothing_while_idle() {
        let mut m = machine();
        assert_eq!(m.mode(), Mode::Idle);
        assert_eq!(m.on_real_input(10, RealInput::Key), (false, None));
        assert_eq!(m.on_real_input(11, RealInput::Button), (false, None));
        assert_eq!(m.mode(), Mode::Idle);
    }

    #[test]
    fn first_input_command_starts_driving_but_read_only_commands_do_not() {
        let mut m = machine();
        m.note_command(5);
        assert_eq!(m.mode(), Mode::Idle);
        m.begin_input(6);
        assert_eq!(m.mode(), Mode::Driven);
    }

    #[test]
    fn a_real_key_switches_and_is_consumed() {
        let mut m = driven();
        assert_eq!(m.on_real_input(100, RealInput::Key), (true, Some(Effect::EnterUser)));
        assert_eq!(m.mode(), Mode::User);
        // Only the first event is swallowed.
        assert_eq!(m.on_real_input(110, RealInput::Key), (false, None));
    }

    #[test]
    fn a_real_click_switches_and_is_consumed() {
        let mut m = driven();
        assert_eq!(m.on_real_input(100, RealInput::Button), (true, Some(Effect::EnterUser)));
    }

    #[test]
    fn a_scroll_switches_without_being_consumed() {
        let mut m = driven();
        assert_eq!(m.on_real_input(100, RealInput::Scroll), (false, Some(Effect::EnterUser)));
    }

    #[test]
    fn pointer_moves_need_a_real_delta() {
        let mut m = driven();
        // The first position only sets the anchor, the pointer merely being there.
        assert_eq!(m.on_real_input(10, RealInput::PointerMove { x: 50.0, y: 50.0 }), (false, None));
        // Repeats of the same spot, and jitter under the threshold, are nothing.
        assert_eq!(m.on_real_input(11, RealInput::PointerMove { x: 50.0, y: 50.0 }), (false, None));
        assert_eq!(m.on_real_input(12, RealInput::PointerMove { x: 51.0, y: 50.5 }), (false, None));
        assert_eq!(m.mode(), Mode::Driven);
        // A real move switches, and is not consumed.
        assert_eq!(
            m.on_real_input(13, RealInput::PointerMove { x: 80.0, y: 50.0 }),
            (false, Some(Effect::EnterUser))
        );
    }

    #[test]
    fn a_slow_drift_adds_up() {
        let mut m = driven();
        m.on_real_input(1, RealInput::PointerMove { x: 10.0, y: 10.0 });
        assert_eq!(m.on_real_input(2, RealInput::PointerMove { x: 11.0, y: 10.0 }), (false, None));
        assert_eq!(
            m.on_real_input(3, RealInput::PointerMove { x: 12.0, y: 10.0 }),
            (false, Some(Effect::EnterUser))
        );
    }

    #[test]
    fn a_space_swipe_does_not_interrupt_driven_work() {
        let mut m = driven();
        // Focus gained, the pointer over the window, then focus lost: all
        // within milliseconds, none of it interaction.
        m.on_focus(1_000, true);
        assert_eq!(m.on_real_input(1_001, RealInput::PointerMove { x: 400.0, y: 300.0 }), (false, None));
        assert_eq!(m.on_real_input(1_002, RealInput::PointerMove { x: 400.0, y: 300.0 }), (false, None));
        m.on_focus(1_010, false);
        assert_eq!(m.tick(1_011), None);
        assert_eq!(m.tick(2_000), None);
        assert_eq!(m.mode(), Mode::Driven);
        // The agent's next command runs at once.
        let t = m.begin_input(2_001);
        assert_eq!(m.poll(t, 2_001), Poll::Run);
    }

    fn into_user(m: &mut Machine, now: Ms) {
        assert_eq!(m.on_real_input(now, RealInput::Key).1, Some(Effect::EnterUser));
    }

    #[test]
    fn leaving_returns_to_driven_after_the_debounce() {
        let mut m = driven();
        into_user(&mut m, 1_000);
        m.on_focus(1_100, false);
        assert_eq!(m.tick(1_399), None);
        assert_eq!(m.mode(), Mode::User);
        assert_eq!(m.tick(1_400), Some(Effect::EnterDriven));
        assert_eq!(m.mode(), Mode::Driven);
    }

    #[test]
    fn coming_back_within_the_debounce_stays_in_user_mode() {
        let mut m = driven();
        into_user(&mut m, 1_000);
        m.on_focus(1_100, false);
        m.on_focus(1_200, true);
        assert_eq!(m.tick(1_500), None);
        assert_eq!(m.mode(), Mode::User);
    }

    #[test]
    fn the_pointer_leaving_starts_the_debounce_and_interaction_cancels_it() {
        let mut m = driven();
        into_user(&mut m, 1_000);
        m.on_real_input(1_100, RealInput::PointerExit);
        assert_eq!(m.tick(1_400), Some(Effect::EnterDriven));

        let mut m = driven();
        into_user(&mut m, 1_000);
        m.on_real_input(1_100, RealInput::PointerExit);
        m.on_real_input(1_200, RealInput::Key);
        assert_eq!(m.tick(1_500), None);
    }

    #[test]
    fn idle_timeout_returns_to_driven_only_without_interaction() {
        let mut m = driven();
        into_user(&mut m, 1_000);
        // Interaction keeps resetting the timeout.
        m.on_real_input(5_000, RealInput::Scroll);
        assert_eq!(m.tick(9_999), None);
        assert_eq!(m.tick(10_000), Some(Effect::EnterDriven));
    }

    #[test]
    fn a_still_pointer_is_not_interaction_in_user_mode() {
        let mut m = driven();
        m.on_real_input(0, RealInput::PointerMove { x: 5.0, y: 5.0 });
        into_user(&mut m, 1_000);
        m.on_real_input(4_000, RealInput::PointerMove { x: 5.0, y: 5.0 });
        assert_eq!(m.tick(6_000), Some(Effect::EnterDriven));
    }

    #[test]
    fn the_agent_going_quiet_ends_driving() {
        let mut m = driven();
        assert_eq!(m.tick(119_999), None);
        assert_eq!(m.tick(120_000), Some(Effect::EnterIdle));
        assert_eq!(m.mode(), Mode::Idle);
        // Nothing is consumed from then on.
        assert_eq!(m.on_real_input(130_000, RealInput::Key), (false, None));
    }

    #[test]
    fn polling_mode_does_not_keep_the_agent_driving() {
        use super::super::commands::parse_line;
        let mut m = driven();
        // The client reads `mode` every 10s, as the shepherd does; the gate
        // notes a command only when its kind counts as activity.
        let mut effect = None;
        let mut t = 10_000;
        while t <= 300_000 && effect.is_none() {
            let kind = classify(&parse_line("mode").unwrap());
            if counts_as_activity(kind) {
                m.note_command(t);
            }
            effect = m.tick(t);
            t += 10_000;
        }
        assert_eq!(effect, Some(Effect::EnterIdle));
        assert_eq!(m.mode(), Mode::Idle);
        for line in ["sync", "size", "shot /x.png"] {
            assert!(!counts_as_activity(classify(&parse_line(line).unwrap())), "{line}");
        }
        for line in ["quit", "key a", "act button:Go"] {
            assert!(counts_as_activity(classify(&parse_line(line).unwrap())), "{line}");
        }
        for line in ["tree", "wait-for button:Go", "coverage"] {
            assert_eq!(classify(&parse_line(line).unwrap()), Kind::Observe, "{line}");
            assert!(!counts_as_activity(Kind::Observe), "{line}");
        }
        assert_eq!(classify(&parse_line("act button:Go").unwrap()), Kind::Input);
    }

    #[test]
    fn any_command_keeps_the_agent_driving() {
        let mut m = driven();
        m.note_command(100_000);
        assert_eq!(m.tick(200_000), None);
        assert_eq!(m.tick(220_000), Some(Effect::EnterIdle));
    }

    #[test]
    fn a_waiting_command_runs_after_a_clean_restore() {
        let mut m = driven();
        into_user(&mut m, 1_000);
        let t = m.begin_input(1_500);
        assert_eq!(m.poll(t, 1_600), Poll::Pending);
        assert_eq!(m.tick(6_000), Some(Effect::EnterDriven));
        // Decided but not yet restored: still waiting.
        assert_eq!(m.poll(t, 6_001), Poll::Pending);
        m.restored(vec![]);
        assert_eq!(m.poll(t, 6_002), Poll::Run);
    }

    #[test]
    fn a_waiting_command_gets_the_conflict_when_the_data_changed() {
        let mut m = driven();
        into_user(&mut m, 1_000);
        let t = m.begin_input(1_500);
        assert_eq!(m.tick(6_000), Some(Effect::EnterDriven));
        m.restored(vec![
            "request 3f2a answered".into(),
            "ENG-2925 pr_draft→approved".into(),
        ]);
        assert_eq!(
            m.poll(t, 6_002),
            Poll::Conflict("request 3f2a answered; ENG-2925 pr_draft→approved".into())
        );
        // The resent command runs: the agent has been told.
        let again = m.begin_input(6_100);
        assert_eq!(m.poll(again, 6_100), Poll::Run);
    }

    #[test]
    fn a_command_that_arrived_after_the_restore_is_not_blamed_for_it() {
        let mut m = driven();
        into_user(&mut m, 1_000);
        assert_eq!(m.tick(6_000), Some(Effect::EnterDriven));
        m.restored(vec!["FindingsPanel closed".into()]);
        let t = m.begin_input(6_001);
        assert_eq!(m.poll(t, 6_001), Poll::Run);
    }

    #[test]
    fn a_waiting_command_gives_up_after_the_cap() {
        let mut m = driven();
        into_user(&mut m, 1_000);
        // The user keeps interacting the whole time.
        let t = m.begin_input(2_000);
        for now in (3_000..62_000).step_by(1_000) {
            m.on_real_input(now, RealInput::Scroll);
            assert_eq!(m.poll(t, now), Poll::Pending, "at {now}");
        }
        m.on_real_input(61_999, RealInput::Scroll);
        assert_eq!(m.poll(t, 62_000), Poll::UserMode);
        assert_eq!(m.mode(), Mode::User);
    }

    #[test]
    fn a_command_while_driven_runs_at_once() {
        let mut m = driven();
        let t = m.begin_input(10);
        assert_eq!(m.poll(t, 10), Poll::Run);
    }

    #[test]
    fn a_command_after_going_idle_starts_driving_again() {
        let mut m = driven();
        assert_eq!(m.tick(200_000), Some(Effect::EnterIdle));
        let t = m.begin_input(200_001);
        assert_eq!(m.mode(), Mode::Driven);
        assert_eq!(m.poll(t, 200_001), Poll::Run);
    }

    #[test]
    fn going_idle_while_the_user_has_control_restores_nothing() {
        let mut m = driven();
        into_user(&mut m, 100_000);
        m.on_real_input(119_000, RealInput::Key);
        assert_eq!(m.tick(120_000), Some(Effect::EnterIdle));
        assert_eq!(m.mode(), Mode::Idle);
    }

    #[test]
    fn the_whole_round_trip() {
        let mut m = driven();
        let (consume, effect) = m.on_real_input(10_000, RealInput::Key);
        assert!(consume);
        assert_eq!(effect, Some(Effect::EnterUser));
        // The user works, then leaves; the agent's command waited all along.
        let t = m.begin_input(10_500);
        m.on_real_input(11_000, RealInput::Scroll);
        m.on_focus(12_000, false);
        assert_eq!(m.tick(12_300), Some(Effect::EnterDriven));
        m.restored(vec![]);
        assert_eq!(m.poll(t, 12_301), Poll::Run);
        assert_eq!(m.mode(), Mode::Driven);
    }
}
