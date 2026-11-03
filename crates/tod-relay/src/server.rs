//! The WebSocket server: sessions for `/exec`, long-lived agents for `/agent/<name>`.

use crate::hold::Hold;
use crate::liveness;
use crate::pty;
use crate::reaper;
use crate::tunnel::Tunnel;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::CStr;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

pub type Out = UnboundedSender<Message>;
pub type WsIn = SplitStream<WebSocketStream<TcpStream>>;

/// Output held for a detached session or agent before it is given up on. A
/// client that missed output cannot recover the stream, so past this the
/// process is ended and the client sees it exit.
const MAX_HELD_BYTES: usize = 32 << 20;
/// How long an agent may sit with no client attached, nothing in flight and no
/// output before it is ended. A client that ends an agent says `bye`, but one
/// that is killed or crashes cannot, and its agent (and the adapter's own
/// children) would otherwise stay forever: the name is the client's, so no
/// later client replaces it. A live client leaves an agent idle for far less
/// (tod releases an idle session after 15 minutes) and resumes the session by
/// id if it finds the process gone.
const DETACHED_AGENT_TTL: Duration = Duration::from_secs(60 * 60);
/// How often detached agents are checked against [`DETACHED_AGENT_TTL`].
const DETACHED_AGENT_SWEEP: Duration = Duration::from_secs(60);
const LOG_DIR: &str = "/opt/tod/logs";
/// How long a poke's own bridging lease (`Hold::lease("poke", ..)`) holds the
/// sandbox awake by itself, giving the supervisor time to take its own hold.
const POKE_BRIDGE_SECS: u64 = 60;
/// The hold a poke takes (not namespaced: only the relay takes it).
const POKE_REASON: &str = "poke";
/// What the supervisor calls its own hold (`POST /hold?reason=supervisor`).
const SUPERVISOR_REASON: &str = "supervisor";

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

struct Relay {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    agents: Mutex<HashMap<String, Arc<Agent>>>,
    tunnel: Arc<Tunnel>,
    hold: Hold,
    home: String,
    /// The command a poke starts when no supervisor is running
    /// (`--supervisor-cmd`; by default the one `tod_sandbox::node` installs,
    /// `/opt/tod/tod-supervisor wake --workspace <the checkout>`). A shell
    /// command line.
    supervisor_cmd: String,
    /// The pid of a supervisor this relay started, with the `poke_gen` it has
    /// seen, while it is still running. `None` both before one has been started and once it exits.
    supervisor: Mutex<Option<(u32, u64)>>,
    /// Counts pokes. A supervisor records the count when it is started or
    /// last signalled; one that exits with the count ahead of that missed a
    /// poke (it arrived in its exit tail), and a new one is started.
    poke_gen: AtomicU64,
    /// What only the supervisor is given: [`take_supervisor_env`].
    supervisor_env: Vec<(String, String)>,
    /// Where supervisors keep their state, one directory per node
    /// (`--supervisor-state-root`): their lock and progress marker, which
    /// [`liveness`] reads.
    supervisor_root: PathBuf,
    /// When the supervisor in `supervisor` was started.
    supervisor_since: Mutex<Option<Instant>>,
    /// A supervisor is being adopted ([`adopt`]); not twice at once.
    adopting: AtomicBool,
}

/// How often the supervisor's liveness is looked at ([`liveness_tick`]).
const LIVENESS_EVERY: Duration = Duration::from_secs(15);
/// How long an adopted supervisor has to end after `SIGTERM` before `SIGKILL`.
const ADOPT_TERM_WAIT: Duration = Duration::from_secs(15);

/// The relay's own environment variables under this prefix are for the
/// supervisor alone (`tod_sandbox::node::RELAY_SUPERVISOR_ENV_PREFIX`): e.g.
/// `TOD_SUPERVISOR_ENV_CLAUDE_CODE_OAUTH_TOKEN` becomes the supervisor's
/// `CLAUDE_CODE_OAUTH_TOKEN`.
pub const SUPERVISOR_ENV_PREFIX: &str = "TOD_SUPERVISOR_ENV_";

/// Takes every `TOD_SUPERVISOR_ENV_<NAME>` out of this process's environment,
/// as `(<NAME>, value)`, so no shell or agent the relay starts inherits it;
/// only [`poke`] hands them on, to the supervisor. Call before any thread
/// starts: removing environment variables is only sound then.
fn take_supervisor_env() -> Vec<(String, String)> {
    let taken = supervisor_env_from(std::env::vars());
    for (name, _) in &taken {
        // SAFETY: called first thing in `main`, before the runtime or any
        // other thread exists.
        unsafe { std::env::remove_var(format!("{SUPERVISOR_ENV_PREFIX}{name}")) };
    }
    taken
}

/// The supervisor's variables among `vars`, prefix removed.
fn supervisor_env_from(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    vars.filter_map(|(key, value)| {
        let name = key.strip_prefix(SUPERVISOR_ENV_PREFIX)?;
        (!name.is_empty()).then(|| (name.to_string(), value))
    })
    .collect()
}

pub fn main(args: &[String]) {
    let supervisor_env = take_supervisor_env();
    reaper::start();
    let mut port = 2222u16;
    let mut max_hold = 4 * 3600u64;
    let mut tunnel_port = crate::tunnel::DEFAULT_PORT;
    let mut supervisor_root = PathBuf::from("/var/lib/tod-supervisor");
    let mut supervisor_cmd = concat!(
        "/opt/tod/tod-supervisor wake --workspace \"",
        "$(if [ -d /workspace/repo/.git ]; then echo /workspace/repo; else echo /workspace; fi)",
        "\""
    )
    .to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => port = it.next().and_then(|v| v.parse().ok()).unwrap_or(port),
            "--max-hold-secs" => max_hold = it.next().and_then(|v| v.parse().ok()).unwrap_or(max_hold),
            "--tunnel-port" => tunnel_port = it.next().and_then(|v| v.parse().ok()).unwrap_or(tunnel_port),
            "--supervisor-state-root" => {
                if let Some(v) = it.next() {
                    supervisor_root = PathBuf::from(v);
                }
            }
            "--supervisor-cmd" => {
                supervisor_cmd = it.next().cloned().unwrap_or(supervisor_cmd);
            }
            _ => {}
        }
    }
    let _ = std::fs::create_dir_all(LOG_DIR);
    let relay = Arc::new(Relay {
        sessions: Mutex::new(HashMap::new()),
        agents: Mutex::new(HashMap::new()),
        tunnel: Arc::new(Tunnel::default()),
        hold: Hold::new(max_hold),
        home: home_dir(),
        supervisor_cmd,
        supervisor: Mutex::new(None),
        poke_gen: AtomicU64::new(0),
        supervisor_env,
        supervisor_root,
        supervisor_since: Mutex::new(None),
        adopting: AtomicBool::new(false),
    });
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    rt.block_on(async move {
        let listener = TcpListener::bind(("0.0.0.0", port)).await.expect("bind relay port");
        log!("tod-relay {} listening on {port}, home {}", crate::VERSION, relay.home);
        tokio::spawn(relay.tunnel.clone().listen(tunnel_port));
        tokio::spawn(liveness_loop(relay.clone()));
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(handle(relay.clone(), stream));
        }
    });
}

/// The process API starts us with `HOME=/blaxel`; commands should see the user's
/// real home, where tools keep their state (e.g. Zed's `~/.zed_server`).
fn home_dir() -> String {
    // SAFETY: getpwuid returns a pointer into static storage or null.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_dir.is_null() {
            return CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned();
        }
    }
    "/root".into()
}

/// The user's own configured shell (`/etc/passwd`), for the no-`cmd` case:
/// bash, then sh, if it isn't set or isn't runnable here.
fn default_shell() -> String {
    // SAFETY: getpwuid returns a pointer into static storage or null.
    let from_passwd: Option<String> = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            Some(CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned())
        } else {
            None
        }
    };
    if let Some(shell) = from_passwd.filter(|shell| Path::new(shell).is_file()) {
        return shell;
    }
    if Path::new("/bin/bash").exists() { "/bin/bash".into() } else { "/bin/sh".into() }
}

fn close(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.to_string().into() }))
}

// ---------------------------------------------------------------------------
// POST /poke: wakes the node's supervisor, starting it if it is not running.

/// Peeks the request line without consuming it. A `POST .../poke` is handled
/// here, outside the WebSocket upgrade, and `None` is returned since the
/// connection is done; anything else is handed back untouched so the caller
/// can still try the WebSocket handshake on it.
async fn try_poke(relay: &Arc<Relay>, stream: TcpStream) -> Option<TcpStream> {
    let mut buf = [0u8; 512];
    let n = match stream.peek(&mut buf).await {
        Ok(n) => n,
        Err(_) => return Some(stream),
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    let Some(line) = head.lines().next() else { return Some(stream) };
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else { return Some(stream) };
    if method != "POST" && method != "GET" {
        return Some(stream);
    }
    // The proxy may or may not strip its `/port/<n>` prefix, so match segments.
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if method == "GET" {
        // Only `GET /holds` is plain HTTP; every other GET is a WebSocket upgrade.
        if segs.last() != Some(&"holds") {
            return Some(stream);
        }
        handle_http(relay, stream, HttpAction::Holds).await;
        return None;
    }
    let action = match segs.last() {
        Some(&"poke") => HttpAction::Poke,
        Some(&"release-all") => HttpAction::ReleaseAll,
        Some(&"hold") => {
            let reason = query_param(query, "reason").unwrap_or_default();
            let secs = query_param(query, "secs").and_then(|s| s.parse::<u64>().ok());
            match secs {
                Some(secs) if !reason.is_empty() && secs > 0 => HttpAction::Hold(reason, secs),
                _ => HttpAction::Bad("hold needs reason=<r>&secs=<n>"),
            }
        }
        Some(&"release") => match query_param(query, "reason") {
            Some(reason) if !reason.is_empty() => HttpAction::Release(reason),
            _ => HttpAction::Bad("release needs reason=<r>"),
        },
        _ => return Some(stream),
    };
    handle_http(relay, stream, action).await;
    None
}

/// A plain-HTTP request on the relay's port (see `try_poke`).
enum HttpAction {
    Poke,
    /// `POST /hold?reason=<r>&secs=<n>`: take or renew a leased hold.
    Hold(String, u64),
    /// `POST /release?reason=<r>`: end it now.
    Release(String),
    /// `POST /release-all`: end every hold now (the watchdog's).
    ReleaseAll,
    /// `GET /holds`: what holds the sandbox, as JSON (`hold::HoldsReport`).
    Holds,
    Bad(&'static str),
}

/// Holds are namespaced so an HTTP caller cannot clear the relay's own.
fn external_reason(reason: &str) -> String {
    format!("ext:{reason}")
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')).collect())
}

async fn handle_http(relay: &Arc<Relay>, mut stream: TcpStream, action: HttpAction) {
    // Drain enough of the request to be a well-behaved HTTP peer; nothing in
    // it (headers or body) matters to a poke.
    let mut buf = vec![0u8; 4096];
    let mut total = Vec::new();
    while total.len() < 65536 {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                total.extend_from_slice(&buf[..n]);
                if total.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let json_reply;
    let reply: &[u8] = match action {
        HttpAction::Holds => {
            let body = serde_json::to_string(&relay.hold.report()).unwrap_or_else(|_| "{}".into());
            json_reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            json_reply.as_bytes()
        }
        HttpAction::ReleaseAll => {
            log!("relay: release-all");
            relay.hold.release_all();
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        }
        HttpAction::Poke => {
            poke(relay);
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        }
        HttpAction::Hold(reason, secs) => {
            relay.hold.lease(&external_reason(&reason), secs);
            if reason == SUPERVISOR_REASON {
                // The supervisor holds for itself now: the poke's bridge has
                // done its job.
                relay.hold.release(POKE_REASON);
            }
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        }
        HttpAction::Release(reason) => {
            relay.hold.release(&external_reason(&reason));
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        }
        HttpAction::Bad(why) => {
            log!("relay: bad request: {why}");
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        }
    };
    let _ = stream.write_all(reply).await;
}

/// Returns true if a process with this pid is (still) around. Used to decide
/// whether a poke should signal the supervisor we started before, or start a
/// fresh one because it is gone.
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence/permission, sends nothing.
    if unsafe { libc::kill(pid as i32, 0) } != 0 {
        return false;
    }
    // A process that exited but is not yet reaped still takes a signal.
    !std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| is_zombie_stat(&s)).unwrap_or(false)
}

/// Whether a `/proc/<pid>/stat` line says the process is a zombie. The
/// command name is in parentheses and may hold anything, so the state is the
/// first field after the last `)`.
fn is_zombie_stat(stat: &str) -> bool {
    stat.rsplit_once(')').and_then(|(_, rest)| rest.trim_start().chars().next()) == Some('Z')
}

/// Wakes the node's supervisor: if one we started is still running, signal it
/// (`SIGUSR1`) to look again now rather than at its next schedule; otherwise
/// start it fresh with `supervisor_cmd` (`keepAlive: false` — it takes its
/// own hold as soon as it starts, see `doc/cloud-sandboxes/
/// autonomous-nodes.md`).
///
/// Either way, a poke takes a short leased hold of its own first: the
/// supervisor needs a moment after starting to take its own hold, and this
/// bridges that gap without granting an indefinite
/// one — it lapses on its own (`POKE_BRIDGE_SECS`) unless another poke
/// arrives to renew it.
fn poke(relay: &Arc<Relay>) {
    relay.hold.lease(POKE_REASON, POKE_BRIDGE_SECS);
    let gen_now = relay.poke_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let mut sup = relay.supervisor.lock().unwrap();
    if let Some((pid, seen)) = sup.as_mut() {
        if alive(*pid) {
            // SAFETY: signalling a process we started ourselves.
            unsafe { libc::kill(*pid as i32, libc::SIGUSR1) };
            *seen = gen_now;
            log!("poke: signalled supervisor pid {pid}");
            return;
        }
        *sup = None;
    }
    // One the relay did not start (a schedule's fallback ran it): replace it
    // with one it tracks, rather than start a second that would only find the
    // lock held.
    match liveness::find(&relay.supervisor_root, std::time::SystemTime::now(), &is_supervisor_process) {
        Ok(Some(found)) => {
            adopt(relay, found.pid);
            return;
        }
        Ok(None) => {}
        Err(e) => {
            log!("poke: cannot look for a running supervisor: {e}");
            crate::problem_limited(
                tod_problems::Level::Error,
                &format!("poke: cannot look for a running supervisor in {}: {e}", relay.supervisor_root.display()),
            );
        }
    }
    start_supervisor(relay, &mut sup);
}

/// Whether `pid` is a live `tod-supervisor` (not a reused pid).
fn is_supervisor_process(pid: u32) -> bool {
    alive(pid)
        && std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|c| String::from_utf8_lossy(&c).contains("tod-supervisor"))
            .unwrap_or(false)
}

/// Looks at the supervisor every [`LIVENESS_EVERY`], for the life of the relay.
async fn liveness_loop(relay: Arc<Relay>) {
    loop {
        tokio::time::sleep(LIVENESS_EVERY).await;
        liveness_tick(&relay, &is_supervisor_process);
    }
}

/// One look ([`liveness::decide`]): kills a supervisor this relay started
/// that has made no progress for [`liveness::STALL_LIMIT`], and adopts one it
/// did not start. A failure to look is reported, never taken for "all well".
fn liveness_tick(relay: &Arc<Relay>, is_supervisor: &dyn Fn(u32) -> bool) {
    let tracked = {
        let sup = relay.supervisor.lock().unwrap();
        let since = relay.supervisor_since.lock().unwrap();
        match (*sup, *since) {
            (Some((pid, _)), Some(at)) if alive(pid) => Some((pid, at.elapsed())),
            _ => None,
        }
    };
    let found = match liveness::find(&relay.supervisor_root, std::time::SystemTime::now(), is_supervisor) {
        Ok(found) => found,
        Err(e) => {
            log!("liveness: cannot read {}: {e}", relay.supervisor_root.display());
            crate::problem_limited(
                tod_problems::Level::Error,
                &format!("liveness: cannot read the supervisor's state in {}: {e}", relay.supervisor_root.display()),
            );
            return;
        }
    };
    match liveness::decide(tracked, found.as_ref(), liveness::STALL_LIMIT) {
        liveness::Action::Nothing => {}
        liveness::Action::KillStalled { pid, age } => {
            let msg = format!(
                "supervisor pid {pid} made no progress for {} min; killed it (its holds are released; the dead-man wake starts the next)",
                age.as_secs() / 60
            );
            log!("liveness: {msg}");
            crate::problem(tod_problems::Level::Error, &msg);
            // SAFETY: a signal to the supervisor this relay started.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        liveness::Action::Adopt { pid } => adopt(relay, pid),
    }
}

/// Replaces a supervisor the relay did not start with one it does: ends it
/// (`SIGTERM`, then `SIGKILL`), and starts another once it is gone, unless one
/// was started meanwhile. Off the async threads: it waits.
fn adopt(relay: &Arc<Relay>, pid: u32) {
    if relay.adopting.swap(true, Ordering::SeqCst) {
        return;
    }
    log!("liveness: supervisor pid {pid} was not started by this relay; replacing it with one it tracks");
    // A bridge for the gap: the replacement needs a moment to hold for itself.
    relay.hold.lease(POKE_REASON, POKE_BRIDGE_SECS);
    let relay = relay.clone();
    tokio::task::spawn_blocking(move || {
        // SAFETY: a signal to the untracked supervisor the lock file names.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        let deadline = Instant::now() + ADOPT_TERM_WAIT;
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if alive(pid) {
            // SAFETY: as above.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            std::thread::sleep(Duration::from_millis(500));
        }
        let mut sup = relay.supervisor.lock().unwrap();
        if sup.is_some_and(|(p, _)| alive(p)) {
            log!("liveness: a supervisor was started meanwhile; not starting another");
        } else {
            *sup = None;
            start_supervisor(&relay, &mut sup);
        }
        relay.adopting.store(false, Ordering::SeqCst);
    });
}

/// Whether a supervisor that has seen `seen` pokes missed one by exiting.
fn missed_poke(poke_gen: u64, seen: u64) -> bool {
    poke_gen > seen
}

/// Starts the supervisor (`supervisor_cmd`) and records it in `sup`, which
/// the caller holds locked so no second one starts meanwhile. Its exit
/// releases the holds it and its poke took (a crashed supervisor leaves its
/// lease behind otherwise), and starts another at once if a poke arrived that
/// it never saw.
fn start_supervisor(relay: &Arc<Relay>, sup: &mut Option<(u32, u64)>) {
    let started_gen = relay.poke_gen.load(Ordering::SeqCst);
    let mut cmd = tokio::process::Command::new("/bin/sh");
    // `exec`, so the pid is the supervisor's own and the next poke's
    // SIGUSR1 reaches it: a shell in between would die of the signal and
    // leave the supervisor running untracked.
    cmd.arg("-c").arg(format!("exec {}", relay.supervisor_cmd));
    cmd.env("HOME", &relay.home);
    cmd.envs(relay.supervisor_env.iter().map(|(k, v)| (k, v)));
    // An ignored signal stays ignored across `exec`, so a poke that lands
    // before the supervisor installed its handler is dropped, not fatal
    // (the default action for SIGUSR1 is to terminate). Its handler replaces
    // this once installed.
    // SAFETY: only async-signal-safe `signal` between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGUSR1, libc::SIG_IGN);
            Ok(())
        });
    }
    match reaper::spawn_tracked(|| cmd.spawn(), |c| c.id()) {
        Ok(mut child) => {
            let pid = child.id().unwrap_or(0);
            *sup = Some((pid, started_gen));
            *relay.supervisor_since.lock().unwrap() = Some(Instant::now());
            log!("poke: started supervisor pid {pid}: {}", relay.supervisor_cmd);
            let relay = relay.clone();
            tokio::spawn(async move {
                let code = child.wait().await.map(exit_code).unwrap_or(255);
                reaper::untrack(pid);
                log!("poke: supervisor pid {pid} exited {code}");
                if code != 0 {
                    crate::problem(tod_problems::Level::Warn, &format!("poke: supervisor pid {pid} exited {code}"));
                }
                relay.hold.release(POKE_REASON);
                relay.hold.release(&external_reason(SUPERVISOR_REASON));
                let mut sup = relay.supervisor.lock().unwrap();
                let Some((cur, seen)) = *sup else { return };
                if cur != pid {
                    return;
                }
                *sup = None;
                let gen_now = relay.poke_gen.load(Ordering::SeqCst);
                if missed_poke(gen_now, seen) {
                    log!("poke: supervisor pid {pid} exited with a poke unanswered; starting another");
                    relay.hold.lease(POKE_REASON, POKE_BRIDGE_SECS);
                    start_supervisor(&relay, &mut sup);
                }
            });
        }
        Err(e) => {
            log!("poke: could not start supervisor: {e}");
            crate::problem_limited(tod_problems::Level::Error, &format!("poke: could not start supervisor: {e}"));
        }
    }
}

async fn handle(relay: Arc<Relay>, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let Some(stream) = try_poke(&relay, stream).await else { return };
    let mut path = String::new();
    let callback = |req: &Request, resp: Response| {
        path = req.uri().path().to_string();
        Ok(resp)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await else { return };
    let (mut sink, mut incoming) = ws.split();
    let (tx, mut rx) = unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let last = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || last {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let first = match incoming.next().await {
        Some(Ok(Message::Text(t))) => t,
        _ => return,
    };
    // The proxy may or may not strip its `/port/<n>` prefix, so match segments.
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(i) = segs.iter().position(|s| *s == "agent") {
        let name = segs.get(i + 1).copied().unwrap_or("default").to_string();
        agent_conn(relay, name, &first, tx, incoming).await;
    } else if segs.contains(&"tunnel") {
        let conn = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
        relay.tunnel.clone().serve(conn, tx, incoming).await;
    } else if segs.contains(&"exec") {
        exec_conn(relay, &first, tx, incoming).await;
    } else {
        let _ = tx.send(close(1008, "unknown path"));
    }
}

// ---------------------------------------------------------------------------
// /exec

#[derive(Deserialize)]
struct ExecReq {
    cmd: Option<String>,
    session: Option<String>,
    /// `[cols, rows]`: run on a pseudo-terminal.
    pty: Option<(u16, u16)>,
    #[serde(default)]
    env: HashMap<String, String>,
    cwd: Option<String>,
    /// Hold the sandbox awake while this command runs.
    #[serde(default)]
    keep_awake: bool,
}

enum Input {
    Data(Vec<u8>),
    Eof,
    Kill,
    Resize(u16, u16),
}

struct Session {
    id: Option<String>,
    key: String,
    input: UnboundedSender<Input>,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    client: Option<(u64, Out)>,
    held: VecDeque<Message>,
    held_bytes: usize,
    exit: Option<i32>,
    busy: bool,
    overflow: bool,
}

fn msg_len(m: &Message) -> usize {
    match m {
        Message::Binary(b) => b.len(),
        Message::Text(t) => t.len(),
        _ => 0,
    }
}

impl Session {
    fn emit(&self, m: Message) {
        let mut s = self.state.lock().unwrap();
        if let Some((_, tx)) = &s.client {
            if tx.send(m.clone()).is_ok() {
                return;
            }
            s.client = None;
        }
        if self.id.is_none() || s.overflow {
            return;
        }
        s.held_bytes += msg_len(&m);
        s.held.push_back(m);
        if s.held_bytes > MAX_HELD_BYTES {
            log!("session {}: over {MAX_HELD_BYTES} bytes held while detached; ending it", self.key);
            s.overflow = true;
            s.held.clear();
            s.held_bytes = 0;
            let _ = self.input.send(Input::Kill);
        }
    }

    /// Returns true when the process had already exited (the exit is delivered
    /// and the session is done).
    fn attach(&self, conn: u64, tx: Out) -> bool {
        let mut s = self.state.lock().unwrap();
        if let Some((_, old)) = s.client.take() {
            let _ = old.send(close(4001, "attached elsewhere"));
        }
        for m in s.held.drain(..) {
            let _ = tx.send(m);
        }
        s.held_bytes = 0;
        if s.busy {
            let _ = tx.send(Message::Text("b1".into()));
        }
        if let Some(code) = s.exit {
            let _ = tx.send(Message::Text(format!("x{code}")));
            let _ = tx.send(close(1000, "exited"));
            return true;
        }
        s.client = Some((conn, tx));
        false
    }

    fn detach(&self, conn: u64) {
        let mut s = self.state.lock().unwrap();
        if s.client.as_ref().is_some_and(|(c, _)| *c == conn) {
            s.client = None;
        }
    }

    fn set_busy(&self, relay: &Relay, busy: bool) {
        let mut s = self.state.lock().unwrap();
        if s.busy == busy {
            return;
        }
        s.busy = busy;
        if let Some((_, tx)) = &s.client {
            let _ = tx.send(Message::Text(if busy { "b1" } else { "b0" }.into()));
        }
        drop(s);
        relay.hold.set(&format!("busy:{}", self.key), busy);
    }

    fn finish(&self, relay: &Relay, code: i32) {
        let mut s = self.state.lock().unwrap();
        s.exit = Some(code);
        s.busy = false;
        let delivered = match s.client.take() {
            Some((_, tx)) => {
                let _ = tx.send(Message::Text(format!("x{code}")));
                let _ = tx.send(close(1000, "exited"));
                true
            }
            None => false,
        };
        drop(s);
        if delivered || self.id.is_none() {
            relay.sessions.lock().unwrap().remove(&self.key);
        }
        relay.hold.set(&format!("busy:{}", self.key), false);
        relay.hold.set(&format!("awake:{}", self.key), false);
    }
}

fn parse_size(s: &str) -> Option<(u16, u16)> {
    let (c, r) = s.split_once('x')?;
    Some((c.parse().ok()?, r.parse().ok()?))
}

async fn exec_conn(relay: Arc<Relay>, first: &str, tx: Out, mut incoming: WsIn) {
    let fail = |tx: &Out, msg: String, code: i32| {
        let _ = tx.send(Message::Text(format!("e{msg}\n")));
        let _ = tx.send(Message::Text(format!("x{code}")));
        let _ = tx.send(close(1000, "exited"));
    };
    let req: ExecReq = match serde_json::from_str(first) {
        Ok(r) => r,
        Err(e) => return fail(&tx, format!("tod-relay: bad request: {e}"), 255),
    };
    let conn = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
    let existing = req.session.as_ref().and_then(|id| relay.sessions.lock().unwrap().get(id).cloned());
    let session = match existing {
        Some(s) => s,
        // Reattaching to a session that has ended and been delivered.
        None if req.session.is_some() && req.cmd.is_none() && req.pty.is_none() => {
            return fail(&tx, "tod-relay: no such session".into(), 255);
        }
        None => match spawn_session(&relay, &req, conn) {
            Ok(s) => s,
            Err(e) => return fail(&tx, format!("tod-relay: could not start: {e}"), 127),
        },
    };
    if session.attach(conn, tx) {
        relay.sessions.lock().unwrap().remove(&session.key);
        return;
    }
    while let Some(msg) = incoming.next().await {
        let input = match msg {
            Ok(Message::Binary(b)) => Input::Data(b),
            Ok(Message::Text(t)) => match t.as_str() {
                "eof" => Input::Eof,
                "bye" => Input::Kill,
                t if t.starts_with('r') => match parse_size(&t[1..]) {
                    Some((c, r)) => Input::Resize(c, r),
                    None => continue,
                },
                _ => continue,
            },
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        let _ = session.input.send(input);
    }
    session.detach(conn);
    if session.id.is_none() {
        let _ = session.input.send(Input::Kill);
    }
}

fn kill_group(pid: u32) {
    // SAFETY: signalling a process group we started with its own group id.
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

fn command(relay: &Relay, req: &ExecReq) -> std::process::Command {
    let mut cmd = match (&req.cmd, req.pty.is_some()) {
        (Some(c), _) if !c.trim().is_empty() => {
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.arg("-c").arg(c);
            cmd
        }
        _ => {
            let mut cmd = std::process::Command::new(default_shell());
            cmd.arg("-l");
            cmd
        }
    };
    cmd.env("HOME", &relay.home);
    if req.pty.is_some() {
        cmd.env("TERM", "xterm-256color");
    }
    cmd.envs(&req.env);
    let cwd = req.cwd.clone().filter(|d| Path::new(d).is_dir()).unwrap_or_else(|| relay.home.clone());
    cmd.current_dir(cwd);
    cmd
}

fn spawn_session(relay: &Arc<Relay>, req: &ExecReq, conn: u64) -> std::io::Result<Arc<Session>> {
    let key = req.session.clone().unwrap_or_else(|| format!("anon-{conn}"));
    let (itx, mut irx) = unbounded_channel::<Input>();
    let session = Arc::new(Session {
        id: req.session.clone(),
        key: key.clone(),
        input: itx,
        state: Mutex::new(SessionState::default()),
    });
    let mut cmd = command(relay, req);
    let label = req.cmd.clone().unwrap_or_else(|| "(shell)".into());
    log!("exec {key}: {}", label.chars().take(120).collect::<String>());

    if let Some((cols, rows)) = req.pty {
        let pty = reaper::spawn_tracked(|| pty::spawn(cmd, cols, rows), |p| Some(p.child.id()))?;
        let pid = pty.child.id();
        let mut child = pty.child;
        let master = Arc::new(pty.master);
        if req.session.is_some() {
            relay.sessions.lock().unwrap().insert(key.clone(), session.clone());
        }
        // Output.
        {
            let s = session.clone();
            let mut m = master.try_clone()?;
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                loop {
                    match std::io::Read::read(&mut m, &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => s.emit(Message::Binary(buf[..n].to_vec())),
                    }
                }
            });
        }
        // Input.
        {
            let m = master.clone();
            std::thread::spawn(move || {
                while let Some(i) = irx.blocking_recv() {
                    match i {
                        Input::Data(b) => pty::write_all(&m, &b),
                        Input::Eof => pty::write_all(&m, &[4]),
                        Input::Resize(c, r) => pty::resize(&m, c, r),
                        Input::Kill => kill_group(pid),
                    }
                }
            });
        }
        // A foreground job keeps the sandbox awake; an idle prompt does not.
        {
            let (s, r, m) = (session.clone(), relay.clone(), master.clone());
            std::thread::spawn(move || {
                while s.state.lock().unwrap().exit.is_none() {
                    s.set_busy(&r, pty::foreground_job(&m, pid));
                    std::thread::sleep(Duration::from_secs(1));
                }
            });
        }
        let (s, r) = (session.clone(), relay.clone());
        std::thread::spawn(move || {
            let code = child.wait().map(exit_code).unwrap_or(255);
            reaper::untrack(pid);
            std::thread::sleep(Duration::from_millis(100)); // let the last output land first
            s.finish(&r, code);
        });
        return Ok(session);
    }

    cmd.process_group(0);
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = reaper::spawn_tracked(|| cmd.spawn(), |c| c.id())?;
    let pid = child.id().unwrap_or(0);
    if req.session.is_some() {
        relay.sessions.lock().unwrap().insert(key.clone(), session.clone());
    }
    if req.keep_awake {
        relay.hold.set(&format!("awake:{key}"), true);
    }
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let out_task = {
        let s = session.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            while let Ok(n) = stdout.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                s.emit(Message::Binary(buf[..n].to_vec()));
            }
        })
    };
    let err_task = {
        let s = session.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                s.emit(Message::Text(format!("e{}", String::from_utf8_lossy(&buf[..n]))));
            }
        })
    };
    tokio::spawn(async move {
        while let Some(i) = irx.recv().await {
            match i {
                Input::Data(b) => {
                    if let Some(s) = stdin.as_mut() {
                        if s.write_all(&b).await.is_err() {
                            stdin = None;
                        }
                    }
                }
                Input::Eof => stdin = None,
                Input::Kill => kill_group(pid),
                Input::Resize(..) => {}
            }
        }
    });
    let (s, r) = (session.clone(), relay.clone());
    tokio::spawn(async move {
        let status = child.wait().await;
        reaper::untrack(pid);
        let _ = out_task.await;
        let _ = err_task.await;
        s.finish(&r, status.map(exit_code).unwrap_or(255));
    });
    Ok(session)
}

// ---------------------------------------------------------------------------
// /agent/<name>

#[derive(Deserialize)]
struct AgentReq {
    #[serde(default)]
    cmd: String,
    /// Only attach to a running agent; never start one.
    #[serde(default)]
    attach_only: bool,
    /// End an agent already running under this name and start afresh.
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    env: HashMap<String, String>,
    cwd: Option<String>,
}

struct Agent {
    name: String,
    pid: u32,
    input: UnboundedSender<String>,
    state: Mutex<AgentState>,
}

#[derive(Default)]
struct AgentState {
    client: Option<(u64, Out)>,
    held: VecDeque<String>,
    held_bytes: usize,
    /// Requests the client sent that the agent has not answered yet.
    client_pending: HashSet<String>,
    /// Requests the agent sent that the client has not answered yet.
    agent_pending: HashSet<String>,
    /// Since when nothing has happened on an agent with no client attached;
    /// `None` while a client is attached.
    idle_since: Option<std::time::Instant>,
}

impl AgentState {
    /// Whether the agent has been abandoned: no client, nothing in flight, and
    /// nothing heard from it for `ttl`.
    fn abandoned(&self, ttl: Duration) -> bool {
        self.client.is_none()
            && self.client_pending.is_empty()
            && self.agent_pending.is_empty()
            && self.idle_since.is_some_and(|since| since.elapsed() >= ttl)
    }
}

/// Tracks JSON-RPC requests in flight: a message with a method and an id is a
/// request from its sender; one with an id and no method answers the other side.
fn track(line: &str, sender_requests: &mut HashSet<String>, other_requests: &mut HashSet<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
    let Some(id) = v.get("id").filter(|i| !i.is_null()) else { return };
    if v.get("method").is_some() {
        sender_requests.insert(id.to_string());
    } else {
        other_requests.remove(&id.to_string());
    }
}

impl Agent {
    fn from_agent(&self, relay: &Relay, line: String) {
        let mut s = self.state.lock().unwrap();
        let st = &mut *s;
        track(&line, &mut st.agent_pending, &mut st.client_pending);
        let mut sent = false;
        if let Some((_, tx)) = &st.client {
            sent = tx.send(Message::Text(line.clone())).is_ok();
            if !sent {
                st.client = None;
            }
        }
        if !sent {
            st.idle_since = Some(std::time::Instant::now());
            st.held_bytes += line.len();
            st.held.push_back(line);
            if st.held_bytes > MAX_HELD_BYTES {
                log!("agent {}: over {MAX_HELD_BYTES} bytes held while detached; ending it", self.name);
                st.held.clear();
                st.held_bytes = 0;
                kill_group(self.pid);
            }
        }
        drop(s);
        self.update_hold(relay);
    }

    fn from_client(&self, relay: &Relay, line: String) {
        {
            let mut s = self.state.lock().unwrap();
            let st = &mut *s;
            track(&line, &mut st.client_pending, &mut st.agent_pending);
        }
        let _ = self.input.send(line);
        self.update_hold(relay);
    }

    /// Awake while the agent owes the client an answer, unless it is itself
    /// waiting on a client that is gone: then it cannot make progress anyway.
    fn update_hold(&self, relay: &Relay) {
        let s = self.state.lock().unwrap();
        let on = !s.client_pending.is_empty() && (s.client.is_some() || s.agent_pending.is_empty());
        drop(s);
        relay.hold.set(&format!("agent:{}", self.name), on);
    }

    fn attach(&self, conn: u64, tx: Out) {
        let mut s = self.state.lock().unwrap();
        if let Some((_, old)) = s.client.take() {
            let _ = old.send(close(4001, "attached elsewhere"));
        }
        for line in s.held.drain(..) {
            let _ = tx.send(Message::Text(line));
        }
        s.held_bytes = 0;
        s.idle_since = None;
        s.client = Some((conn, tx));
    }

    fn detach(&self, conn: u64) {
        let mut s = self.state.lock().unwrap();
        if s.client.as_ref().is_some_and(|(c, _)| *c == conn) {
            s.client = None;
            s.idle_since = Some(std::time::Instant::now());
        }
    }
}

async fn agent_conn(relay: Arc<Relay>, name: String, first: &str, tx: Out, mut incoming: WsIn) {
    let conn = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
    let req: AgentReq = match serde_json::from_str(first) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(close(1008, &format!("bad request: {e}")));
            return;
        }
    };
    let mut existing = relay.agents.lock().unwrap().get(&name).cloned();
    if req.replace {
        if let Some(old) = existing.take() {
            log!("agent {name}: replaced");
            relay.agents.lock().unwrap().remove(&name);
            if let Some((_, old_tx)) = old.state.lock().unwrap().client.take() {
                let _ = old_tx.send(close(4001, "replaced"));
            }
            kill_group(old.pid);
            relay.hold.set(&format!("agent:{name}"), false);
        }
    }
    let agent = match existing {
        Some(a) => a,
        None if req.attach_only || req.cmd.trim().is_empty() => {
            let _ = tx.send(close(4004, "no such agent"));
            return;
        }
        None => {
            match spawn_agent(&relay, &name, &req) {
                Ok(a) => a,
                Err(e) => {
                    let _ = tx.send(close(1011, &format!("could not start: {e}")));
                    return;
                }
            }
        }
    };
    agent.attach(conn, tx);
    agent.update_hold(&relay);
    while let Some(msg) = incoming.next().await {
        match msg {
            // Agent lines are JSON, so this cannot be one.
            Ok(Message::Text(t)) if t == "bye" => {
                kill_group(agent.pid);
                break;
            }
            Ok(Message::Text(t)) => agent.from_client(&relay, t),
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    agent.detach(conn);
    agent.update_hold(&relay);
}

fn spawn_agent(relay: &Arc<Relay>, name: &str, req: &AgentReq) -> std::io::Result<Arc<Agent>> {
    let exec = ExecReq {
        cmd: Some(req.cmd.clone()),
        session: None,
        pty: None,
        env: req.env.clone(),
        cwd: req.cwd.clone(),
        keep_awake: false,
    };
    let mut cmd = command(relay, &exec);
    cmd.process_group(0);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{LOG_DIR}/agent-{name}.log"))?;
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(log));
    let mut child = reaper::spawn_tracked(|| cmd.spawn(), |c| c.id())?;
    let pid = child.id().unwrap_or(0);
    log!("agent {name}: started pid {pid}: {}", req.cmd);
    let (itx, mut irx) = unbounded_channel::<String>();
    let agent = Arc::new(Agent {
        name: name.to_string(),
        pid,
        input: itx,
        state: Mutex::new(AgentState::default()),
    });
    relay.agents.lock().unwrap().insert(name.to_string(), agent.clone());
    let (a, r, n) = (agent.clone(), relay.clone(), name.to_string());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(DETACHED_AGENT_SWEEP).await;
            let current = r.agents.lock().unwrap().get(&n).is_some_and(|cur| Arc::ptr_eq(cur, &a));
            if !current {
                break;
            }
            if a.state.lock().unwrap().abandoned(DETACHED_AGENT_TTL) {
                log!("agent {n}: abandoned for {DETACHED_AGENT_TTL:?}; ending it");
                // Its exit task removes it and releases its hold.
                kill_group(a.pid);
                break;
            }
        }
    });
    let mut stdin = child.stdin.take().expect("stdin");
    tokio::spawn(async move {
        while let Some(line) = irx.recv().await {
            let line = line.trim_end();
            if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });
    let stdout = child.stdout.take().expect("stdout");
    let (a, r) = (agent.clone(), relay.clone());
    let out_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            a.from_agent(&r, line);
        }
    });
    let (a, r, name) = (agent.clone(), relay.clone(), name.to_string());
    tokio::spawn(async move {
        let code = child.wait().await.map(exit_code).unwrap_or(255);
        reaper::untrack(pid);
        let _ = out_task.await;
        log!("agent {name}: exited {code}");
        // A replaced agent's name already belongs to its successor.
        let current = {
            let mut agents = r.agents.lock().unwrap();
            let current = agents.get(&name).is_some_and(|cur| Arc::ptr_eq(cur, &a));
            if current {
                agents.remove(&name);
            }
            current
        };
        if let Some((_, tx)) = a.state.lock().unwrap().client.take() {
            let _ = tx.send(close(4000, &format!("exit:{code}")));
        }
        if current {
            r.hold.set(&format!("agent:{name}"), false);
        }
    });
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_env_is_the_prefixed_variables_without_the_prefix() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("TOD_SUPERVISOR_ENV_CLAUDE_CODE_OAUTH_TOKEN", "tok"),
            ("TOD_SUPERVISOR_ENV_", "ignored"),
            ("TOD_SUPERVISOR_AGENT", "mock"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()));
        assert_eq!(
            supervisor_env_from(vars),
            [("CLAUDE_CODE_OAUTH_TOKEN".to_string(), "tok".to_string())]
        );
    }
}

#[cfg(test)]
mod abandoned_tests {
    use super::*;

    #[test]
    fn an_agent_is_abandoned_only_when_idle_detached_and_owed_nothing() {
        let mut st = AgentState::default();
        // Never detached (a client is about to attach): not abandoned.
        assert!(!st.abandoned(Duration::ZERO));
        st.idle_since = Some(std::time::Instant::now());
        assert!(st.abandoned(Duration::ZERO));
        assert!(!st.abandoned(Duration::from_secs(3600)));
        st.client_pending.insert("1".into());
        assert!(!st.abandoned(Duration::ZERO));
        st.client_pending.clear();
        st.agent_pending.insert("2".into());
        assert!(!st.abandoned(Duration::ZERO));
    }

    #[test]
    fn zombie_state_is_read_after_the_last_paren() {
        assert!(is_zombie_stat("123 (sleep) Z 1 123 123 0 -1"));
        assert!(!is_zombie_stat("123 (sleep) S 1 123 123 0 -1"));
        assert!(is_zombie_stat("123 (a) Z b) Z 1"));
        assert!(!is_zombie_stat("123 (a) Z b) S 1"));
        assert!(!is_zombie_stat(""));
    }

    #[test]
    fn a_poke_after_the_supervisor_last_saw_the_count_is_missed() {
        assert!(!missed_poke(3, 3));
        assert!(missed_poke(4, 3));
    }

    fn test_relay(cmd: &str) -> Arc<Relay> {
        unsafe { std::env::set_var("TOD_PROBLEMS_DIR", std::env::temp_dir().join("tod-relay-test-problems")) };
        Arc::new(Relay {
            sessions: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            tunnel: Arc::new(Tunnel::default()),
            hold: Hold::new(3600),
            home: "/tmp".into(),
            supervisor_cmd: cmd.into(),
            supervisor: Mutex::new(None),
            poke_gen: AtomicU64::new(0),
            supervisor_env: vec![],
            supervisor_root: std::env::temp_dir().join(format!("tod-relay-test-no-supervisors-{}", std::process::id())),
            supervisor_since: Mutex::new(None),
            adopting: AtomicBool::new(false),
        })
    }

    fn current_pid(relay: &Relay) -> Option<u32> {
        relay.supervisor.lock().unwrap().map(|(p, _)| p)
    }

    /// A signal to a supervisor that has not installed its handler is
    /// ignored, and a poke the supervisor never saw, arriving as it exits,
    /// starts the next one.
    #[test]
    fn a_poke_in_the_exit_tail_starts_another_supervisor() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let relay = test_relay("sleep 0.6");
            poke(&relay);
            let first = current_pid(&relay).expect("started");
            // Another poke signals the live child (SIG_IGN: it survives).
            tokio::time::sleep(Duration::from_millis(100)).await;
            poke(&relay);
            assert_eq!(current_pid(&relay), Some(first));
            assert!(alive(first), "SIGUSR1 must not kill a supervisor before its handler is installed");
            // A poke the child never saw (as in its exit tail): count ahead.
            relay.poke_gen.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(750)).await;
            let second = current_pid(&relay).expect("a new supervisor was started");
            assert_ne!(second, first);
        });
    }

    #[test]
    fn an_exit_with_every_poke_seen_starts_nothing() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let relay = test_relay("sleep 0.3");
            poke(&relay);
            tokio::time::sleep(Duration::from_millis(800)).await;
            assert_eq!(current_pid(&relay), None);
        });
    }

    /// A relay whose supervisor state root holds a lock naming `pid`.
    fn relay_with_untracked(cmd: &str, pid: u32, tag: &str) -> Arc<Relay> {
        let root = std::env::temp_dir().join(format!("tod-relay-adopt-{tag}-{}", std::process::id()));
        let node = root.join("node-1");
        std::fs::create_dir_all(&node).unwrap();
        std::fs::write(node.join(liveness::LOCK_FILE), pid.to_string()).unwrap();
        let relay = test_relay(cmd);
        // Rebuilt with the root: `Relay` has no setter, and tests own it alone.
        Arc::new(Relay {
            sessions: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            tunnel: Arc::new(Tunnel::default()),
            hold: Hold::new(3600),
            home: "/tmp".into(),
            supervisor_cmd: relay.supervisor_cmd.clone(),
            supervisor: Mutex::new(None),
            poke_gen: AtomicU64::new(0),
            supervisor_env: vec![],
            supervisor_root: root,
            supervisor_since: Mutex::new(None),
            adopting: AtomicBool::new(false),
        })
    }

    /// The process API's fallback ran the supervisor itself, untracked and
    /// under a 120 s kill: the relay replaces it with one it tracks.
    #[test]
    fn an_untracked_supervisor_is_replaced_by_one_the_relay_tracks() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let mut untracked = std::process::Command::new("sleep").arg("60").spawn().unwrap();
            let old = untracked.id();
            let relay = relay_with_untracked("sleep 5", old, "replace");
            liveness_tick(&relay, &|p| p == old && alive(p));
            // Ended, and another started that is tracked.
            let mut tracked = None;
            for _ in 0..60 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                tracked = current_pid(&relay);
                if tracked.is_some() {
                    break;
                }
            }
            let new = tracked.expect("the relay started its own supervisor");
            assert_ne!(new, old);
            assert!(untracked.wait().is_ok());
            assert!(!alive(old), "the untracked one was ended");
            // SAFETY: ends the test's own child.
            unsafe { libc::kill(new as i32, libc::SIGKILL) };
        });
    }

    /// A poke that finds an untracked supervisor adopts it rather than start
    /// a second one beside it.
    #[test]
    fn a_poke_adopts_an_untracked_supervisor_instead_of_starting_a_second() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let mut untracked = std::process::Command::new("sleep").arg("60").spawn().unwrap();
            let old = untracked.id();
            let relay = relay_with_untracked("sleep 5", old, "poke");
            // The default check wants a `tod-supervisor` command line, which
            // `sleep` has not: this lock is not a live supervisor, so the poke
            // starts one. With the lock naming a supervisor it adopts.
            assert!(!is_supervisor_process(old));
            poke(&relay);
            let started = current_pid(&relay).expect("a poke with no supervisor starts one");
            assert_ne!(started, old);
            assert!(alive(old), "a pid that is not a supervisor is left alone");
            untracked.kill().unwrap();
            let _ = untracked.wait();
            // SAFETY: ends the test's own child.
            unsafe { libc::kill(started as i32, libc::SIGKILL) };
        });
    }

    /// A supervisor the relay started that stops marking progress is killed.
    #[test]
    fn a_tracked_supervisor_with_a_stale_progress_marker_is_killed() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let relay = test_relay("sleep 30");
            poke(&relay);
            let pid = current_pid(&relay).expect("started");
            // Started long ago, never marked anything.
            *relay.supervisor_since.lock().unwrap() = Some(Instant::now() - liveness::STALL_LIMIT - Duration::from_secs(5));
            liveness_tick(&relay, &|_| false);
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(!alive(pid), "a wedged supervisor must be killed");
        });
    }
}
