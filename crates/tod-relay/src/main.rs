//! `tod-relay`: the one server tod runs inside a cloud sandbox.
//!
//! It listens for WebSockets on one port (reached through the sandbox
//! provider's port proxy, which authenticates every request) and routes by path:
//!
//! - `/exec`: run a command, or open a terminal, with its stdio over the socket.
//!   With a session id the process outlives the socket, and output meanwhile is
//!   held for the next attach.
//! - `/agent/<name>`: attach to one long-lived, line-oriented agent process
//!   (JSON-RPC, e.g. ACP), started on first attach.
//! - `/tunnel`: carry connections to a loopback port here (2223) to the
//!   client, so `tod-cli` in the sandbox reaches the app on the client's machine.
//!
//! `POST /poke` (plain HTTP, not a WebSocket, on the same port) wakes the
//! node's supervisor, starting it (`--supervisor-cmd`) if it is not already
//! running, or signalling it (`SIGUSR1`) if it is.
//!
//! While a terminal runs a foreground job, a non-interactive command asked to
//! keep the sandbox awake runs, or an agent answers a request, the relay holds
//! the sandbox awake through the provider's local API, so work never freezes
//! mid-way just because no client is connected. Everything else lets it sleep.
//!
//! The sandbox's init does not reap orphans, so the relay is a child
//! subreaper and reaps the processes orphaned below it (`reaper`).
//!
//! The wire protocol is in `doc/cloud-sandboxes/relay-protocol.md`.

#[macro_use]
mod log;
#[cfg(target_os = "linux")]
mod hold;
#[allow(dead_code)]
mod liveness;
#[cfg(target_os = "linux")]
mod pty;
#[cfg(target_os = "linux")]
mod reaper;
#[cfg(target_os = "linux")]
mod server;
#[cfg(target_os = "linux")]
mod tunnel;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Record a failure where the node's supervisor will forward it to the app's
/// log panel (it does so only when it is already talking to the orchestrator;
/// this never contacts anything). The caller keeps its own stderr line.
#[allow(dead_code)]
pub(crate) fn problem(level: tod_problems::Level, message: &str) {
    if let Err(err) = tod_problems::report_in(&tod_problems::node_dir(), "tod-relay", level, None, message) {
        log!("tod-relay: could not record this in the problem log: {err}");
    }
}

/// How long a problem report with the same message is held back after one
/// went out: a failure that repeats every second (the process API down) is
/// one line a minute in the log panel, not sixty.
const PROBLEM_REPEAT: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether the report of `key` is due at `now`, noting it when it is.
fn report_due(
    seen: &mut std::collections::HashMap<String, std::time::Instant>,
    key: &str,
    now: std::time::Instant,
) -> bool {
    match seen.get(key) {
        Some(at) if now.saturating_duration_since(*at) < PROBLEM_REPEAT => false,
        _ => {
            seen.insert(key.to_string(), now);
            true
        }
    }
}

/// [`problem`], at most once a minute per distinct message.
#[allow(dead_code)]
pub(crate) fn problem_limited(level: tod_problems::Level, message: &str) {
    static SEEN: std::sync::Mutex<Option<std::collections::HashMap<String, std::time::Instant>>> =
        std::sync::Mutex::new(None);
    let due = report_due(SEEN.lock().unwrap().get_or_insert_with(Default::default), message, std::time::Instant::now());
    if due {
        problem(level, message);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("tod-relay {VERSION}");
        return;
    }
    #[cfg(target_os = "linux")]
    server::main(&args);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        eprintln!("tod-relay only runs inside a Linux sandbox");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_repeated_problem_is_reported_once_a_minute_per_message() {
        let t0 = Instant::now();
        let mut seen = Default::default();
        assert!(report_due(&mut seen, "a", t0));
        assert!(!report_due(&mut seen, "a", t0 + Duration::from_secs(1)));
        assert!(!report_due(&mut seen, "a", t0 + Duration::from_secs(59)));
        assert!(report_due(&mut seen, "b", t0 + Duration::from_secs(1)));
        assert!(report_due(&mut seen, "a", t0 + Duration::from_secs(60)));
        assert!(!report_due(&mut seen, "a", t0 + Duration::from_secs(61)));
    }
}
