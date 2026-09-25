//! The WebSocket server: sessions for `/exec`, long-lived agents for `/agent/<name>`.

use crate::hold::Hold;
use crate::pty;
use crate::tunnel::Tunnel;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::CStr;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
const LOG_DIR: &str = "/opt/tod/logs";

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

struct Relay {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    agents: Mutex<HashMap<String, Arc<Agent>>>,
    tunnel: Arc<Tunnel>,
    hold: Hold,
    home: String,
}

pub fn main(args: &[String]) {
    let mut port = 2222u16;
    let mut max_hold = 4 * 3600u64;
    let mut tunnel_port = crate::tunnel::DEFAULT_PORT;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => port = it.next().and_then(|v| v.parse().ok()).unwrap_or(port),
            "--max-hold-secs" => max_hold = it.next().and_then(|v| v.parse().ok()).unwrap_or(max_hold),
            "--tunnel-port" => tunnel_port = it.next().and_then(|v| v.parse().ok()).unwrap_or(tunnel_port),
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
    });
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    rt.block_on(async move {
        let listener = TcpListener::bind(("0.0.0.0", port)).await.expect("bind relay port");
        eprintln!("tod-relay {} listening on {port}, home {}", crate::VERSION, relay.home);
        tokio::spawn(relay.tunnel.clone().listen(tunnel_port));
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

fn close(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.to_string().into() }))
}

async fn handle(relay: Arc<Relay>, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
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
            eprintln!("session {}: over {MAX_HELD_BYTES} bytes held while detached; ending it", self.key);
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
            let shell = if Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
            let mut cmd = std::process::Command::new(shell);
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
    eprintln!("exec {key}: {}", label.chars().take(120).collect::<String>());

    if let Some((cols, rows)) = req.pty {
        let pty = pty::spawn(cmd, cols, rows)?;
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
    let mut child = cmd.spawn()?;
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
            st.held_bytes += line.len();
            st.held.push_back(line);
            if st.held_bytes > MAX_HELD_BYTES {
                eprintln!("agent {}: over {MAX_HELD_BYTES} bytes held while detached; ending it", self.name);
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
        s.client = Some((conn, tx));
    }

    fn detach(&self, conn: u64) {
        let mut s = self.state.lock().unwrap();
        if s.client.as_ref().is_some_and(|(c, _)| *c == conn) {
            s.client = None;
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
            eprintln!("agent {name}: replaced");
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
    let mut child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    eprintln!("agent {name}: started pid {pid}: {}", req.cmd);
    let (itx, mut irx) = unbounded_channel::<String>();
    let agent = Arc::new(Agent {
        name: name.to_string(),
        pid,
        input: itx,
        state: Mutex::new(AgentState::default()),
    });
    relay.agents.lock().unwrap().insert(name.to_string(), agent.clone());
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
        let _ = out_task.await;
        eprintln!("agent {name}: exited {code}");
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
