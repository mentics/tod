//! A line-oriented agent (ACP) in a sandbox, bridged to this process's stdio,
//! so that to whoever started this process it looks like a local agent.
//!
//! The agent runs under the relay's `/agent/<name>` and outlives the socket.
//! The bridge stays attached while a request is in flight either way (a
//! prompt turn, a permission request) and for `idle` after, then detaches so
//! the sandbox can sleep; the next line on stdin wakes it and reattaches. The
//! relay holds whatever the agent writes while detached.
//!
//! While attached it can also carry the relay's tunnel to a port on this
//! machine (`tod-cli`, see [`crate::tunnel`]) and keep a file in place, which
//! tod's Zed shim reads as "keep Zed attached to this sandbox".
//!
//! Stdin closing ends the agent. The bridge exits when the agent does, with
//! its exit code.

use crate::relay::{self, Ws};
use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

pub struct BridgeOptions {
    /// The agent's name on the relay; one agent per name.
    pub name: String,
    /// Command line, run with `sh -c` in the sandbox.
    pub cmd: String,
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
    /// Detach after this long with nothing in flight.
    pub idle: Duration,
    /// Carry the relay's tunnel to this local port while attached.
    pub tunnel_port: Option<u16>,
    /// A file to keep in place (and fresh) while attached.
    pub awake_file: Option<PathBuf>,
}

/// JSON-RPC requests in flight: a message with a method and an id is a
/// request from its sender; one with an id and no method answers the other side.
fn track(line: &str, sender_requests: &mut HashSet<String>, other_requests: &mut HashSet<String>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    let Some(id) = v.get("id").filter(|i| !i.is_null()) else { return };
    if v.get("method").is_some() {
        sender_requests.insert(id.to_string());
    } else {
        other_requests.remove(&id.to_string());
    }
}

/// What the bridge holds while attached.
struct Attached {
    ws: Ws,
    tunnel: Option<tokio::task::JoinHandle<()>>,
}

impl Attached {
    fn release(self, awake_file: &Option<PathBuf>) {
        if let Some(t) = self.tunnel {
            t.abort();
        }
        if let Some(f) = awake_file {
            let _ = std::fs::remove_file(f);
        }
    }
}

pub async fn run(sandbox_url: &str, token: &str, opts: BridgeOptions) -> Result<i32> {
    let url = relay::ws_url(sandbox_url, &format!("/agent/{}", opts.name));
    let attach = |first: Value| {
        let (url, sandbox_url, token) = (url.clone(), sandbox_url.to_string(), token.to_string());
        let (tunnel_port, awake_file) = (opts.tunnel_port, opts.awake_file.clone());
        async move {
            // The tunnel first, so the agent's first `tod-cli` finds it.
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let tunnel = tunnel_port.map(|port| {
                let (sandbox_url, token) = (sandbox_url.clone(), token.clone());
                tokio::spawn(async move {
                    let mut ready = Some(ready_tx);
                    // Reconnect while attached: the relay may restart under us.
                    loop {
                        let _ = crate::tunnel::run(&sandbox_url, &token, port, ready.take()).await;
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                })
            });
            if tunnel.is_some() {
                let _ = tokio::time::timeout(Duration::from_secs(20), ready_rx).await;
            }
            let mut ws = match relay::connect(&url, &token).await {
                Ok(ws) => ws,
                Err(e) => {
                    if let Some(t) = tunnel {
                        t.abort();
                    }
                    return Err(e);
                }
            };
            ws.send(Message::text(first.to_string())).await?;
            if let Some(f) = &awake_file {
                if let Some(dir) = f.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(f, b"");
            }
            anyhow::Ok(Attached { ws, tunnel })
        }
    };

    // Lines from stdin, from a plain thread (see `relay::run_stdio`).
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Option<String>>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            let read = std::io::BufRead::read_line(&mut stdin.lock(), &mut line);
            let item = match read {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
            };
            let end = item.is_none();
            if in_tx.send(item).is_err() || end {
                break;
            }
        }
    });

    let first = json!({
        "cmd": opts.cmd,
        "env": opts.env,
        "cwd": opts.cwd,
        // A fresh bridge means a fresh agent: an earlier one under this name
        // was left behind by a bridge that did not get to end it.
        "replace": true,
    });
    let mut attached = Some(attach(first).await.context("start the agent in the sandbox")?);
    let reattach = json!({ "attach_only": true });
    let mut client_pending: HashSet<String> = HashSet::new();
    let mut agent_pending: HashSet<String> = HashSet::new();
    let mut last_activity = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut stdout = std::io::stdout();

    loop {
        tokio::select! {
            line = in_rx.recv() => {
                let Some(Some(line)) = line else {
                    // Stdin closed: end the agent.
                    if attached.is_none() {
                        attached = attach(reattach.clone()).await.ok();
                    }
                    if let Some(mut a) = attached.take() {
                        let _ = a.ws.send(Message::text("bye")).await;
                        let _ = a.ws.close(None).await;
                        a.release(&opts.awake_file);
                    }
                    return Ok(0);
                };
                last_activity = Instant::now();
                if attached.is_none() {
                    attached = Some(reattach_with_retry(&attach, &reattach).await?);
                }
                track(&line, &mut client_pending, &mut agent_pending);
                let a = attached.as_mut().expect("attached");
                if a.ws.send(Message::text(line.clone())).await.is_err() {
                    // Sent into a dead socket: reattach and send it again.
                    if let Some(a) = attached.take() {
                        a.release(&opts.awake_file);
                    }
                    let mut a = reattach_with_retry(&attach, &reattach).await?;
                    a.ws.send(Message::text(line)).await.context("send to the agent")?;
                    attached = Some(a);
                }
            }
            msg = async { attached.as_mut().expect("attached").ws.next().await }, if attached.is_some() => {
                match msg {
                    Some(Ok(Message::Text(line))) => {
                        last_activity = Instant::now();
                        track(&line, &mut agent_pending, &mut client_pending);
                        stdout.write_all(line.as_bytes())?;
                        stdout.write_all(b"\n")?;
                        stdout.flush()?;
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let a = attached.take().expect("attached");
                        a.release(&opts.awake_file);
                        let (code, reason) = frame
                            .map(|f| (f.code, f.reason.to_string()))
                            .unwrap_or((CloseCode::Normal, String::new()));
                        match u16::from(code) {
                            4000 => {
                                let exit = reason.strip_prefix("exit:").and_then(|c| c.parse().ok()).unwrap_or(1);
                                if exit != 0 {
                                    eprintln!(
                                        "tod-sandbox: the agent exited with code {exit}; its output is in \
                                         /opt/tod/logs/agent-{}.log in the sandbox",
                                        opts.name
                                    );
                                }
                                return Ok(exit);
                            }
                            4001 => return Err(anyhow!("another connection took over this agent")),
                            4004 => return Err(anyhow!("the agent is no longer running in the sandbox")),
                            _ if code != CloseCode::Normal && code != CloseCode::Away => {
                                return Err(anyhow!("the relay refused the agent: {reason}"));
                            }
                            // The socket went (the relay restarted, or the proxy
                            // dropped it): reattach at once if work is in flight.
                            _ if !client_pending.is_empty() || !agent_pending.is_empty() => {
                                attached = Some(reattach_with_retry(&attach, &reattach).await?);
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => {
                        if let Some(a) = attached.take() {
                            a.release(&opts.awake_file);
                        }
                        if !client_pending.is_empty() || !agent_pending.is_empty() {
                            attached = Some(reattach_with_retry(&attach, &reattach).await?);
                        }
                    }
                }
            }
            _ = tick.tick() => {
                // Kept fresh, so one left behind by a crash goes stale.
                if let (Some(_), Some(f)) = (&attached, &opts.awake_file) {
                    if f.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a > Duration::from_secs(30))) {
                        let _ = std::fs::write(f, b"");
                    }
                }
                let idle = client_pending.is_empty() && agent_pending.is_empty()
                    && last_activity.elapsed() >= opts.idle;
                if idle {
                    if let Some(mut a) = attached.take() {
                        let _ = a.ws.close(None).await;
                        a.release(&opts.awake_file);
                    }
                }
            }
        }
    }
}

/// Reattach to a running agent, retrying for a while: a waking sandbox or a
/// restarting relay can refuse the first tries.
async fn reattach_with_retry<F, Fut>(attach: &F, first: &Value) -> Result<Attached>
where
    F: Fn(Value) -> Fut,
    Fut: std::future::Future<Output = Result<Attached>>,
{
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match attach(first.clone()).await {
            Ok(a) => return Ok(a),
            Err(e) if Instant::now() > deadline => return Err(e.context("reattach to the agent")),
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_requests_both_ways() {
        let (mut client, mut agent) = (HashSet::new(), HashSet::new());
        track(r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt"}"#, &mut client, &mut agent);
        assert_eq!(client.len(), 1);
        track(r#"{"jsonrpc":"2.0","id":"p","method":"session/request_permission"}"#, &mut agent, &mut client);
        assert_eq!(agent.len(), 1);
        track(r#"{"jsonrpc":"2.0","id":"p","result":{}}"#, &mut client, &mut agent);
        assert!(agent.is_empty());
        track(r#"{"jsonrpc":"2.0","method":"session/update"}"#, &mut agent, &mut client);
        track(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, &mut agent, &mut client);
        assert!(client.is_empty());
    }
}
