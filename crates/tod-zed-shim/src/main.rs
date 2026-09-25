//! `ssh`, `scp`, and `sftp` for a Zed that tod starts.
//!
//! Copies of this one binary sit first on that Zed's PATH (see
//! `tod_store::fleet::code_editor::zed::zed_env`); it acts on the name it was
//! run as. For a host named `<sandbox>.tod`, `ssh` runs Zed's commands over
//! the sandbox relay's WebSocket, with no SSH at all:
//!
//! - Zed's long-lived `proxy` connection is *parked* (its socket closed, so the
//!   sandbox can sleep) after a stretch with nothing but heartbeats. While
//!   parked, Zed's pings are answered here; the first real message reattaches
//!   to the same remote process, whose output the relay held meanwhile.
//! - `-t` (a Zed terminal) opens a relay terminal, parked the same way when idle.
//! - Zed's connection "master" is answered locally.
//!
//! `scp` and `sftp` run the real OpenSSH programs with this `ssh` as their
//! transport. Every other host goes to the real `ssh` untouched.
//!
//! Design: `doc/cloud-sandboxes/blaxel-remote.md`.

mod zed_rpc;

use futures_util::{SinkExt, StreamExt};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tod_sandbox::config;
use tod_sandbox::relay::{self, Event, ExecRequest, Ws};
use tod_sandbox::terminal::{self, TerminalOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Options of OpenSSH's `ssh` that take a value.
const WITH_VALUE: &str = "BbcDEeFIiJLlmOoPpQRSWw";
const SFTP_SERVER: &str = "/opt/tod/bin/sftp-server";

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default()
}

fn log(msg: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(exe_dir().join("shim.log")) {
        let _ = writeln!(f, "{:.3} [{}] {msg}", t.as_secs_f64(), std::process::id());
    }
}

fn fail(msg: &str) -> ! {
    log(msg);
    eprintln!("tod ssh: {msg}");
    std::process::exit(255);
}

/// The real program `name`, from PATH with this directory left out.
fn real_program(name: &str) -> PathBuf {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let own = std::fs::canonicalize(exe_dir()).unwrap_or_else(|_| exe_dir());
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone()) == own {
                continue;
            }
            let candidate = dir.join(&file);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    if cfg!(windows) {
        return PathBuf::from(r"C:\Windows\System32\OpenSSH").join(file);
    }
    PathBuf::from(format!("/usr/bin/{name}"))
}

fn run_real(name: &str, args: &[String], extra: &[String]) -> ! {
    let status = Command::new(real_program(name)).args(extra).args(args).status();
    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(255)),
        Err(e) => fail(&format!("could not run the real {name}: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Arguments

#[derive(Debug, Default, PartialEq)]
struct SshArgs {
    dest: Option<String>,
    command: Vec<String>,
    subsystem: bool,
    tty: bool,
    no_command: bool,
    /// `-O <ctl>`: a request to a control master.
    control: Option<String>,
    control_path: Option<String>,
}

/// Parses ssh's argv. Like OpenSSH, options may also follow the destination.
fn parse_args(args: &[String]) -> SshArgs {
    let mut out = SshArgs::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            if out.dest.is_none() {
                out.dest = args.get(i).cloned();
                i += 1;
            }
            break;
        }
        if !a.starts_with('-') || a.len() < 2 {
            if out.dest.is_none() {
                out.dest = Some(a.clone());
                i += 1;
                continue;
            }
            break;
        }
        let flags: Vec<char> = a[1..].chars().collect();
        for (k, c) in flags.iter().enumerate() {
            match c {
                's' => out.subsystem = true,
                't' => out.tty = true,
                'N' => out.no_command = true,
                _ => {}
            }
            if WITH_VALUE.contains(*c) {
                let attached: String = flags[k + 1..].iter().collect();
                let value = if attached.is_empty() {
                    i += 1;
                    args.get(i).cloned().unwrap_or_default()
                } else {
                    attached
                };
                match c {
                    'O' => out.control = Some(value),
                    'S' => out.control_path = Some(value),
                    'o' => {
                        let (key, val) = value.split_once(['=', ' ']).unwrap_or((&value, ""));
                        if key.eq_ignore_ascii_case("ControlPath") {
                            out.control_path = Some(val.trim().to_string());
                        }
                    }
                    _ => {}
                }
                break;
            }
        }
        i += 1;
    }
    out.command = args.get(i..).map(<[String]>::to_vec).unwrap_or_default();
    out
}

// ---------------------------------------------------------------------------
// Reaching the sandbox

struct Target {
    name: String,
    ws: String,
    token: String,
}

fn tod_sandbox() -> PathBuf {
    std::env::var_os("TOD_SANDBOX_BIN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("tod-sandbox"))
}

fn target(name: &str) -> Target {
    let out = Command::new(tod_sandbox()).args(["connect-info", name]).output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(o) => fail(&format!("tod-sandbox connect-info {name}: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) => fail(&format!("could not run tod-sandbox ({}): {e}", tod_sandbox().display())),
    };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let (Some(url), Some(token)) = (v["url"].as_str(), v["token"].as_str()) else {
        fail("tod-sandbox connect-info gave no url and token");
    };
    Target { name: name.to_string(), ws: relay::ws_url(url, "/exec"), token: token.to_string() }
}

/// Connects to the relay; if that fails, has `tod-sandbox` bring the sandbox
/// up to date (and start the relay) once, then tries again.
async fn connect(t: &Target) -> Ws {
    match relay::connect(&t.ws, &t.token).await {
        Ok(ws) => ws,
        Err(first) => {
            log(&format!("connect failed ({first:#}); running tod-sandbox ensure {}", t.name));
            let _ = Command::new(tod_sandbox()).args(["ensure", &t.name]).status();
            relay::connect(&t.ws, &t.token)
                .await
                .unwrap_or_else(|e| fail(&format!("cannot reach sandbox {}: {e:#}", t.name)))
        }
    }
}

// ---------------------------------------------------------------------------

fn main() {
    let exe = std::env::current_exe().unwrap_or_default();
    let prog = exe.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if prog == "scp" || prog == "sftp" {
        log(&format!("{prog} {args:?}"));
        let own_ssh = exe.with_file_name(format!("ssh{}", std::env::consts::EXE_SUFFIX));
        run_real(&prog, &args, &["-S".to_string(), own_ssh.display().to_string()]);
    }

    let parsed = parse_args(&args);
    let Some(name) = parsed.dest.as_deref().and_then(config::name_for_host).map(str::to_string) else {
        run_real("ssh", &args, &[]);
    };

    if let Some(ctl) = &parsed.control {
        // Nothing to control: there is no master connection to check or stop.
        log(&format!("control request {ctl}: answered locally"));
        std::process::exit(0);
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    if parsed.no_command && parsed.command.is_empty() {
        rt.block_on(master(parsed.control_path));
    }
    let cmd = if parsed.subsystem {
        match parsed.command.first().map(String::as_str) {
            Some("sftp") => SFTP_SERVER.to_string(),
            other => fail(&format!("unsupported subsystem {other:?}")),
        }
    } else {
        parsed.command.join(" ")
    };
    if cmd.contains("ZED_SSH_CONNECTION_ESTABLISHED") {
        // Zed's Windows "master" only has to print this and stay alive; holding
        // no connection here is what lets the sandbox sleep.
        println!("ZED_SSH_CONNECTION_ESTABLISHED");
        let _ = std::io::stdout().flush();
        log(&format!("{name}: master answered locally"));
        rt.block_on(std::future::pending::<()>());
    }
    let t = target(&name);
    let code = if parsed.tty && !parsed.subsystem {
        rt.block_on(tty(t, cmd))
    } else if cmd.contains(" proxy ") && cmd.contains("--identifier") {
        rt.block_on(proxy(t, cmd))
    } else {
        rt.block_on(exec(t, cmd))
    };
    std::process::exit(code);
}

/// Zed's control master on macOS and Linux: it waits for the control socket
/// to exist, then runs each command with `-o ControlPath=...`, which this shim
/// answers directly. A placeholder file stands in for the socket.
async fn master(control_path: Option<String>) -> ! {
    if let Some(p) = &control_path {
        let _ = std::fs::write(p, b"");
    }
    log(&format!("master answered locally (control path {control_path:?})"));
    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 1024];
    while !matches!(stdin.read(&mut buf).await, Ok(0) | Err(_)) {}
    if let Some(p) = &control_path {
        let _ = std::fs::remove_file(p);
    }
    std::process::exit(0)
}

async fn exec(t: Target, cmd: String) -> i32 {
    let started = Instant::now();
    // Resolve the sandbox first, so a first-time `ensure` happens here and not
    // mid-command.
    drop(connect(&t).await);
    let req = ExecRequest::command(cmd.clone());
    let code = relay::run_stdio(&t.ws, &t.token, &req).await.unwrap_or_else(|e| fail(&format!("{e:#}")));
    log(&format!("{}: exec {}ms exit={code} {}", t.name, started.elapsed().as_millis(), &cmd[..cmd.len().min(100)]));
    code
}

async fn tty(t: Target, cmd: String) -> i32 {
    drop(connect(&t).await);
    let park = std::env::var("TOD_TERMINAL_PARK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(60u64);
    log(&format!("{}: terminal {}", t.name, &cmd[..cmd.len().min(100)]));
    let opts = TerminalOptions {
        cmd: (!cmd.trim().is_empty()).then_some(cmd),
        cwd: None,
        // Zed's terminals do not carry `tod-cli`; tod's own do.
        sandbox_url: String::new(),
        env: Default::default(),
        tunnel_port: None,
        park_after: (park > 0).then(|| Duration::from_secs(park)),
        log,
    };
    terminal::run(&t.ws, &t.token, opts).await.unwrap_or_else(|e| fail(&format!("{e:#}")))
}

/// Zed's proxy connection, parked when idle.
// `attach!` also runs just before a return, where its bookkeeping goes unread.
#[allow(unused_assignments)]
async fn proxy(t: Target, cmd: String) -> i32 {
    let park_after = Duration::from_secs(
        std::env::var("TOD_ZED_PARK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30),
    );
    let session = format!(
        "zed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
    );
    // While tod works in the sandbox (an agent turn), each of its agents
    // keeps a file fresh in this directory, so edits and diagnostics reach
    // Zed live.
    let hold_flag = exe_dir().join("awake").join(&t.name);
    log(&format!("{}: proxy start {session} park_after={park_after:?}", t.name));

    // Frames from Zed (stdin), each with its 4-byte length prefix.
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        loop {
            let mut len = [0u8; 4];
            if stdin.read_exact(&mut len).await.is_err() {
                break;
            }
            let mut f = len.to_vec();
            f.resize(4 + u32::from_le_bytes(len) as usize, 0);
            if stdin.read_exact(&mut f[4..]).await.is_err() {
                break;
            }
            if in_tx.send(f).is_err() {
                break;
            }
        }
    });

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut ws: Option<Ws> = None;
    let mut from_server: Vec<u8> = Vec::new();
    let mut last_server_id = 0u32;
    let mut last_real = Instant::now();
    let mut faked: u32 = 0;
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    let first = ExecRequest { cmd: Some(cmd), session: Some(session.clone()), ..ExecRequest::default() };
    let again = ExecRequest::reattach(&session);
    macro_rules! attach {
        ($req:expr, $why:expr) => {{
            let started = Instant::now();
            let mut s = connect(&t).await;
            if s.send($req.message()).await.is_err() {
                fail("the relay closed the connection");
            }
            log(&format!("{}: attach ({}) {}ms, faked {faked} pings while parked", t.name, $why, started.elapsed().as_millis()));
            faked = 0;
            ws = Some(s);
        }};
    }
    attach!(first, "start");

    loop {
        tokio::select! {
            frame = in_rx.recv() => {
                let Some(frame) = frame else {
                    // Zed closed the proxy: end the remote process too.
                    if ws.is_none() { attach!(again, "close"); }
                    let _ = ws.as_mut().unwrap().send(Message::text("bye")).await;
                    log(&format!("{}: proxy end (zed closed stdin)", t.name));
                    return 0;
                };
                let head = zed_rpc::envelope_head(&frame[4..]);
                if ws.is_none() && head.payload == Some(zed_rpc::PING) {
                    let _ = stdout.write_all(&zed_rpc::ack_frame(last_server_id, head.id)).await;
                    let _ = stdout.flush().await;
                    faked += 1;
                    continue;
                }
                if !head.is_heartbeat() { last_real = Instant::now(); }
                if ws.is_none() { attach!(again, format!("zed sent field {:?}", head.payload)); }
                if ws.as_mut().unwrap().send(Message::binary(frame)).await.is_err() {
                    log("send failed; reattaching");
                    ws = None;
                }
            }
            msg = async { ws.as_mut().unwrap().next().await }, if ws.is_some() => {
                let Some(Ok(msg)) = msg else {
                    log(&format!("{}: socket dropped; parked", t.name));
                    ws = None;
                    continue;
                };
                match relay::parse(msg) {
                    Event::Stdout(b) => {
                        from_server.extend_from_slice(&b);
                        while from_server.len() >= 4 {
                            let n = u32::from_le_bytes(from_server[..4].try_into().unwrap()) as usize;
                            if from_server.len() < 4 + n { break; }
                            let f: Vec<u8> = from_server.drain(..4 + n).collect();
                            let head = zed_rpc::envelope_head(&f[4..]);
                            last_server_id = head.id;
                            if !head.is_heartbeat() { last_real = Instant::now(); }
                            let _ = stdout.write_all(&f).await;
                        }
                        let _ = stdout.flush().await;
                    }
                    Event::Stderr(s) => { let _ = stderr.write_all(s.as_bytes()).await; }
                    Event::Exit(code) => {
                        log(&format!("{}: remote proxy exited {code}", t.name));
                        return code;
                    }
                    Event::Closed => { log(&format!("{}: relay closed; parked", t.name)); ws = None; }
                    Event::Busy(_) | Event::Other => {}
                }
            }
            _ = tick.tick() => {
                let hold = held_awake(&hold_flag);
                if hold && ws.is_none() { attach!(again, "tod: sandbox busy"); }
                if hold { last_real = Instant::now(); }
                if ws.is_some() && last_real.elapsed() >= park_after && from_server.is_empty() {
                    let _ = ws.as_mut().unwrap().close(None).await;
                    ws = None;
                    log(&format!("{}: parked after {}s idle", t.name, last_real.elapsed().as_secs()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn parses_a_command() {
        let p = parse_args(&args("-q -o ConnectTimeout=10 root@dev.tod -T cd /root && ls"));
        assert_eq!(p.dest.as_deref(), Some("root@dev.tod"));
        assert_eq!(p.command, args("cd /root && ls"));
        assert!(!p.tty);
    }

    #[test]
    fn parses_terminal_and_master_flags() {
        let p = parse_args(&args("-t root@dev.tod bash"));
        assert!(p.tty);
        let p = parse_args(&args("-N -o ControlMaster=yes -o ControlPath=/tmp/x.sock dev.tod"));
        assert!(p.no_command && p.command.is_empty());
        assert_eq!(p.control_path.as_deref(), Some("/tmp/x.sock"));
        let p = parse_args(&args("-O exit -S /tmp/x.sock dev.tod"));
        assert_eq!(p.control.as_deref(), Some("exit"));
        assert_eq!(p.control_path.as_deref(), Some("/tmp/x.sock"));
    }

    #[test]
    fn parses_subsystem() {
        let p = parse_args(&args("-oForwardX11=no -s -- dev.tod sftp"));
        assert!(p.subsystem);
        assert_eq!(p.dest.as_deref(), Some("dev.tod"));
        assert_eq!(p.command, args("sftp"));
    }
}

/// Whether any agent is keeping `dir` fresh: a file there written in the last
/// two minutes. One left behind by an agent that did not get to remove it
/// goes stale on its own.
fn held_awake(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age < Duration::from_secs(120)))
    })
}
