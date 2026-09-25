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
//! While a terminal runs a foreground job, a non-interactive command asked to
//! keep the sandbox awake runs, or an agent answers a request, the relay holds
//! the sandbox awake through the provider's local API, so work never freezes
//! mid-way just because no client is connected. Everything else lets it sleep.
//!
//! The wire protocol is in `doc/cloud-sandboxes/relay-protocol.md`.

#[cfg(target_os = "linux")]
mod hold;
#[cfg(target_os = "linux")]
mod pty;
#[cfg(target_os = "linux")]
mod server;
#[cfg(target_os = "linux")]
mod tunnel;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
