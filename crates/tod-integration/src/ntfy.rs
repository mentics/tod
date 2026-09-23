//! Minimal client for [ntfy.sh](https://ntfy.sh), the relay used to deliver
//! sealed journey bundles (`doc/journeys/spec.md` §9.3).
//!
//! Blocking `reqwest`, matching the rest of this crate. No `tod-*`
//! dependencies here by design (this is a leaf crate) — callers in
//! `tod-core::journey::submit` wrap this in the `Relay` trait.

use anyhow::{Context, Result};
use serde::Deserialize;

/// One message read back from an ntfy topic's `/json?poll=1` feed.
#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(default)]
    pub time: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub attachment: Option<Attachment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub expires: i64,
}

/// Publishes `bytes` as a file attachment named `filename` to `topic` on
/// `server` (e.g. `https://ntfy.sh`).
pub fn publish_file(server: &str, topic: &str, filename: &str, bytes: &[u8]) -> Result<()> {
    let url = format!("{}/{}", server.trim_end_matches('/'), topic);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .put(&url)
        .header("Filename", filename)
        .body(bytes.to_vec())
        .send()
        .with_context(|| format!("PUT {url} failed"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("ntfy PUT {url} returned {status}");
    }
    Ok(())
}

/// Publishes a plain-text message to `topic` on `server`.
pub fn publish_text(server: &str, topic: &str, text: &str) -> Result<()> {
    let url = format!("{}/{}", server.trim_end_matches('/'), topic);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .put(&url)
        .body(text.to_string())
        .send()
        .with_context(|| format!("PUT {url} failed"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("ntfy PUT {url} returned {status}");
    }
    Ok(())
}

/// Polls `topic` on `server` for messages since `since` (a message id, or the
/// literal `"all"` for everything the server still caches).
pub fn poll(server: &str, topic: &str, since: &str) -> Result<Vec<Message>> {
    let url = format!(
        "{}/{}/json?poll=1&since={}",
        server.trim_end_matches('/'),
        topic,
        since
    );
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(&url)
        .send()
        .with_context(|| format!("GET {url} failed"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("ntfy GET {url} returned {status}");
    }
    let body = resp.text().context("reading ntfy poll response body")?;
    let mut messages = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Message = serde_json::from_str(line)
            .with_context(|| format!("parsing ntfy message line: {line}"))?;
        messages.push(msg);
    }
    Ok(messages)
}

/// Downloads the bytes at `url` (an attachment url returned by [`poll`]).
pub fn download(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("GET {url} failed"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("download GET {url} returned {status}");
    }
    let bytes = resp.bytes().context("reading download body")?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    /// A tiny hand-rolled HTTP stub: accepts one connection, records the
    /// request, and replies with a fixed status/body. No mocking crate is
    /// in the workspace yet, so this keeps tests network-free without
    /// adding one.
    struct StubServer {
        addr: String,
        request_rx: mpsc::Receiver<StubRequest>,
    }

    struct StubRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn start_stub(status_line: &'static str, response_body: &'static str) -> StubServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let mut data = Vec::new();
                // Read until we've seen the header terminator; good enough
                // for these small test requests.
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                    if data.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let header_end = data.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(data.len());
                let header_text = String::from_utf8_lossy(&data[..header_end]).to_string();
                let mut lines = header_text.lines();
                let request_line = lines.next().unwrap_or_default().to_string();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let mut headers = Vec::new();
                let mut content_length = 0usize;
                for line in lines {
                    if let Some((k, v)) = line.split_once(':') {
                        let k = k.trim().to_string();
                        let v = v.trim().to_string();
                        if k.eq_ignore_ascii_case("content-length") {
                            content_length = v.parse().unwrap_or(0);
                        }
                        headers.push((k, v));
                    }
                }
                let mut body = data[(header_end + 4).min(data.len())..].to_vec();
                while body.len() < content_length {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n]);
                }
                let _ = tx.send(StubRequest { method, path, headers, body });
                let resp = format!(
                    "{status_line}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{response_body}",
                    response_body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        StubServer { addr: format!("http://{addr}"), request_rx: rx }
    }

    #[test]
    fn publish_file_puts_bytes_with_filename_header() {
        let stub = start_stub("HTTP/1.1 200 OK", "");
        publish_file(&stub.addr, "inbox-topic", "bundle.journey.age", b"hello world").unwrap();
        let req = stub.request_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(req.method, "PUT");
        assert_eq!(req.path, "/inbox-topic");
        assert!(req.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("filename") && v == "bundle.journey.age"));
        assert_eq!(req.body, b"hello world");
    }

    #[test]
    fn publish_text_puts_plain_body() {
        let stub = start_stub("HTTP/1.1 200 OK", "");
        publish_text(&stub.addr, "ack-topic", "got 1234").unwrap();
        let req = stub.request_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(req.method, "PUT");
        assert_eq!(req.body, b"got 1234");
    }

    #[test]
    fn poll_parses_one_json_message_per_line() {
        let body = r#"{"id":"abc123","time":1700000000,"message":"got 42"}"#;
        let stub = start_stub("HTTP/1.1 200 OK", body);
        let msgs = poll(&stub.addr, "ack-topic", "all").unwrap();
        let req = stub.request_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/ack-topic/json?poll=1&since=all");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, "abc123");
        assert_eq!(msgs[0].message, "got 42");
        assert!(msgs[0].attachment.is_none());
    }

    #[test]
    fn poll_parses_attachment_fields() {
        let body = r#"{"id":"a1","time":1,"message":"","attachment":{"name":"bundle.age","url":"http://example/x","size":10,"expires":123}}"#;
        let stub = start_stub("HTTP/1.1 200 OK", body);
        let msgs = poll(&stub.addr, "topic", "all").unwrap();
        let att = msgs[0].attachment.as_ref().unwrap();
        assert_eq!(att.name, "bundle.age");
        assert_eq!(att.url, "http://example/x");
        assert_eq!(att.size, 10);
        assert_eq!(att.expires, 123);
    }

    #[test]
    fn download_returns_body_bytes() {
        let stub = start_stub("HTTP/1.1 200 OK", "raw-bytes-here");
        let bytes = download(&format!("{}/file", stub.addr)).unwrap();
        assert_eq!(bytes, b"raw-bytes-here");
    }

    #[test]
    fn publish_file_errors_on_non_success_status() {
        let stub = start_stub("HTTP/1.1 500 Internal Server Error", "");
        let err = publish_file(&stub.addr, "topic", "f.age", b"x").unwrap_err();
        assert!(err.to_string().contains("500"));
    }
}
