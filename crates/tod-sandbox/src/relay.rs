//! The client side of `tod-relay`'s WebSocket protocol
//! (`doc/cloud-sandboxes/relay-protocol.md`).

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Where a sandbox's relay is reached: `wss://<sandbox>/port/2222<path>`.
pub fn ws_url(sandbox_url: &str, path: &str) -> String {
    let base = sandbox_url.trim_end_matches('/').replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    format!("{base}/port/{}{path}", crate::blaxel::RELAY_PORT)
}

/// TLS needs a process-wide crypto provider; installing it twice is harmless.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub async fn connect(url: &str, token: &str) -> Result<Ws> {
    init_tls();
    let mut req = url.into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {token}").parse()?);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.with_context(|| format!("connect {url}"))?;
    Ok(ws)
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ExecRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pty: Option<(u16, u16)>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub keep_awake: bool,
}

impl ExecRequest {
    pub fn command(cmd: impl Into<String>) -> Self {
        Self { cmd: Some(cmd.into()), ..Self::default() }
    }

    /// Reattaches to a running session; fails with "no such session" once it is gone.
    pub fn reattach(session: impl Into<String>) -> Self {
        Self { session: Some(session.into()), ..Self::default() }
    }

    pub fn message(&self) -> Message {
        Message::text(serde_json::to_string(self).expect("exec request serializes"))
    }
}

/// One message from the relay on an `/exec` socket.
#[derive(Debug, PartialEq)]
pub enum Event {
    Stdout(Vec<u8>),
    Stderr(String),
    Exit(i32),
    /// A terminal's foreground job started (`true`) or ended.
    Busy(bool),
    Closed,
    Other,
}

pub fn parse(msg: Message) -> Event {
    match msg {
        Message::Binary(b) => Event::Stdout(b),
        Message::Text(t) => match t.as_bytes().first() {
            Some(b'e') => Event::Stderr(t[1..].to_string()),
            Some(b'x') => Event::Exit(t[1..].trim().parse().unwrap_or(255)),
            Some(b'b') => Event::Busy(&t[1..] == "1"),
            _ => Event::Other,
        },
        Message::Close(_) => Event::Closed,
        _ => Event::Other,
    }
}

pub fn resize_message(cols: u16, rows: u16) -> Message {
    Message::text(format!("r{cols}x{rows}"))
}

/// Runs a command with this process's stdin, stdout, and stderr; returns its exit code.
pub async fn run_stdio(url: &str, token: &str, req: &ExecRequest) -> Result<i32> {
    let ws = connect(url, token).await?;
    let (mut tx, mut rx) = ws.split();
    tx.send(req.message()).await?;
    // stdin is read on a plain thread: tokio's reader is a blocking task that
    // shutting down the runtime would wait on while stdin stays open.
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Option<Vec<u8>>>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = vec![0u8; 65536];
        loop {
            let chunk = match stdin.read(&mut buf) {
                Ok(0) | Err(_) => None,
                Ok(n) => Some(buf[..n].to_vec()),
            };
            let end = chunk.is_none();
            if in_tx.send(chunk).is_err() || end {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(chunk) = in_rx.recv().await {
            let msg = match chunk {
                Some(bytes) => Message::binary(bytes),
                None => Message::text("eof"),
            };
            if tx.send(msg).await.is_err() {
                break;
            }
        }
        // Keep the sink alive so the socket is not closed under the reader.
        std::future::pending::<()>().await;
    });
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    while let Some(msg) = rx.next().await {
        match parse(msg?) {
            Event::Stdout(b) => {
                stdout.write_all(&b).await?;
                stdout.flush().await?;
            }
            Event::Stderr(s) => {
                stderr.write_all(s.as_bytes()).await?;
                stderr.flush().await?;
            }
            Event::Exit(code) => return Ok(code),
            Event::Closed => break,
            _ => {}
        }
    }
    Err(anyhow!("the relay closed the connection before the command finished"))
}

/// Runs a command with no input and collects its output.
pub async fn run_capture(url: &str, token: &str, cmd: &str) -> Result<(i32, Vec<u8>, String)> {
    let ws = connect(url, token).await?;
    let (mut tx, mut rx) = ws.split();
    tx.send(ExecRequest::command(cmd).message()).await?;
    tx.send(Message::text("eof")).await?;
    let (mut out, mut err) = (Vec::new(), String::new());
    while let Some(msg) = rx.next().await {
        match parse(msg?) {
            Event::Stdout(b) => out.extend(b),
            Event::Stderr(s) => err.push_str(&s),
            Event::Exit(code) => return Ok((code, out, err)),
            Event::Closed => break,
            _ => {}
        }
    }
    Err(anyhow!("the relay closed the connection before the command finished"))
}

/// Quotes `s` for a POSIX shell.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_relay_url() {
        assert_eq!(ws_url("https://sbx-a.bl.run/", "/exec"), "wss://sbx-a.bl.run/port/2222/exec");
    }

    #[test]
    fn exec_request_omits_defaults() {
        let m = ExecRequest::reattach("s1").message();
        assert_eq!(m.into_text().unwrap(), r#"{"session":"s1"}"#);
        let mut r = ExecRequest::command("ls");
        r.pty = Some((80, 24));
        r.keep_awake = true;
        assert_eq!(r.message().into_text().unwrap(), r#"{"cmd":"ls","pty":[80,24],"keep_awake":true}"#);
    }

    #[test]
    fn parses_events() {
        assert_eq!(parse(Message::text("x3")), Event::Exit(3));
        assert_eq!(parse(Message::text("eoops")), Event::Stderr("oops".into()));
        assert_eq!(parse(Message::text("b1")), Event::Busy(true));
        assert_eq!(parse(Message::binary(vec![1])), Event::Stdout(vec![1]));
    }

    #[test]
    fn quotes_for_shell() {
        assert_eq!(shell_quote("/root/tod"), "/root/tod");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
